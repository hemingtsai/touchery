//! Query-to-app similarity.
//!
//! One algorithm scores every candidate. The query is aligned against a
//! prepared search key (`apps::SearchKey`) with a Damerau-style local
//! alignment:
//!
//! * matching a query character at a word start, or right after the previous
//!   match, scores full marks; matching it later in the key costs a little;
//! * skipping key characters is free, which is what makes abbreviations
//!   ("jsq" → 计算器) and missing letters ("sai" → Safari) work;
//! * a query character that cannot be matched, or that is matched by a
//!   different letter, costs a full mark;
//! * every adjacent transposition costs half a mark, so "gmial" still matches
//!   "Gmail" with slightly lower confidence and several swaps cost more.
//!
//! The total is divided by the length of the query, not the key: a short query
//! that is fully matched is a success, so `saf` scores as well against Safari
//! as the full name does. An entry is listed only when its best key reaches a
//! threshold that depends on the query length.

use crate::apps::{AppEntry, SearchKey, ascii_fold, mask_slot};

/// A perfect match, in thousandths.
pub const SCALE: u32 = 1000;

const MATCH_TOKEN_START: i32 = 1000;
const MATCH_CONTIGUOUS: i32 = 1000;
const MATCH_MID: i32 = 800;
const PEN_DELETE: i32 = 1000;
const PEN_TRANSPOSE: i32 = 500;

/// Minimum score, in thousandths, for a query of this many characters: a
/// single character only matches a word start, two allow an abbreviation,
/// three or more tolerate a typo.
fn threshold_for(len: usize) -> u32 {
    match len {
        0 => 0,
        1 => 950,
        2 => 750,
        _ => 600,
    }
}

/// Reused DP rows, sized to the longest key of the current query so the hot
/// path does not allocate per candidate.
#[derive(Default)]
struct Rows {
    /// Values of row i-2 (needed by the transposition move).
    values2: Vec<i32>,
    /// Values of row i-1.
    values1: Vec<i32>,
    /// Values of row i, being written.
    values0: Vec<i32>,
    /// Whether row i-1's cell was reached by matching a character.
    via1: Vec<bool>,
    /// Same, for the row being written.
    via0: Vec<bool>,
}

impl Rows {
    fn prepare(&mut self, len: usize) {
        for values in [&mut self.values2, &mut self.values1, &mut self.values0] {
            values.clear();
            values.resize(len, 0);
        }
        for flags in [&mut self.via1, &mut self.via0] {
            flags.clear();
            flags.resize(len, false);
        }
    }
}

/// The query split into segments of folded bytes: '/' separates segments and
/// everything that folds to nothing (Chinese, spaces, punctuation) is dropped.
fn query_segments(query: &str) -> Vec<Vec<u8>> {
    let mut segments: Vec<Vec<u8>> = vec![Vec::new()];
    for ch in query.chars() {
        if ch == '/' {
            segments.push(Vec::new());
            continue;
        }
        if let Some(byte) = ascii_fold(ch) {
            segments
                .last_mut()
                .expect("there is always a segment")
                .push(byte);
        }
    }
    segments.retain(|segment| !segment.is_empty());
    segments
}

fn query_mask(segment: &[u8]) -> u64 {
    segment
        .iter()
        .fold(0u64, |mask, byte| mask | (1u64 << mask_slot(*byte)))
}

fn base_weight(starts: &[u32], index: usize) -> i32 {
    if starts.binary_search(&(index as u32)).is_ok() {
        MATCH_TOKEN_START
    } else {
        MATCH_MID
    }
}

/// Similarity of one query against one key, in thousandths, or 0 when the key
/// cannot reach `threshold`.
fn similarity(q: &[u8], q_mask: u64, key: &SearchKey, threshold: u32, rows: &mut Rows) -> u32 {
    let n = q.len();
    let m = key.chars.len();
    if n == 0 || m == 0 {
        return 0;
    }

    // A query character the key does not contain at all can only be deleted:
    // it loses its own mark and costs a penalty.
    let absent = i64::from((q_mask & !key.mask).count_ones());
    if absent * 2 * i64::from(SCALE) > (n as i64) * i64::from(SCALE - threshold) {
        return 0;
    }
    // Every matched query character needs a character of the key.
    if 2 * (m as i64) * i64::from(SCALE) < (n as i64) * i64::from(SCALE + threshold) {
        return 0;
    }

    rows.prepare(m + 1);
    let key_chars: &[u8] = &key.chars;
    let starts: &[u32] = &key.starts;

    for i in 1..=n {
        let qi = q[i - 1];
        rows.values0[0] = -(i as i32) * PEN_DELETE;
        rows.via0[0] = false;

        for j in 1..=m {
            // Skipping a key character is free; leaving a query character
            // unmatched costs a full mark.
            let mut best = rows.values0[j - 1].max(rows.values1[j] - PEN_DELETE);
            let mut via_match = false;

            if qi == key_chars[j - 1] {
                let weight = if i >= 2 && j >= 2 && rows.via1[j - 1] {
                    MATCH_CONTIGUOUS
                } else {
                    base_weight(starts, j - 1)
                };
                let candidate = rows.values1[j - 1] + weight;
                if candidate > best {
                    best = candidate;
                    via_match = true;
                }
            }

            // Adjacent transposition: the two characters are swapped in the
            // query. Taking this move several times handles several swapped
            // pairs, each of which costs another PEN_TRANSPOSE.
            if i >= 2 && j >= 2 && qi == key_chars[j - 2] && q[i - 2] == key_chars[j - 1] {
                let candidate =
                    rows.values2[j - 2] + base_weight(starts, j - 2) + base_weight(starts, j - 1)
                        - PEN_TRANSPOSE;
                if candidate > best {
                    best = candidate;
                    via_match = true;
                }
            }

            rows.values0[j] = best;
            rows.via0[j] = via_match;
        }

        std::mem::swap(&mut rows.values2, &mut rows.values1);
        std::mem::swap(&mut rows.values1, &mut rows.values0);
        std::mem::swap(&mut rows.via1, &mut rows.via0);
    }

    // The total is in thousandths per matched character, so dividing by the
    // query length turns it into a score where a fully matched query is SCALE,
    // however long the key is.
    let total = rows.values1[m].clamp(0, (n as i32) * SCALE as i32) as u32;
    (total / n as u32).min(SCALE)
}

/// Best weighted (score, key length) among `keys`, or `None` when none reaches
/// the threshold. Ties prefer the shorter key, i.e. the more specific match.
fn best_key_score(
    q: &[u8],
    q_mask: u64,
    keys: &[SearchKey],
    threshold: u32,
    rows: &mut Rows,
) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    for key in keys {
        let raw = similarity(q, q_mask, key, threshold, rows);
        if raw == 0 {
            continue;
        }
        let weighted = raw * key.kind.weight() / SCALE;
        if weighted < threshold {
            continue;
        }
        let candidate = (weighted, key.chars.len() as u32);
        best = match best {
            Some((score, len))
                if score > candidate.0 || (score == candidate.0 && len <= candidate.1) =>
            {
                Some((score, len))
            }
            _ => Some(candidate),
        };
    }
    best
}

/// Rank the apps matching `query`; the score is in thousandths of a perfect
/// match. Entries below the threshold for the query length are omitted, so a
/// one-character query only lists apps whose word starts with it. With
/// `apps_only`, apps outside an Applications root are skipped.
pub fn search_apps(query: &str, apps: &[AppEntry], apps_only: bool) -> Vec<(usize, u32)> {
    let wanted = |app: &AppEntry| !apps_only || app.in_app_dir;

    let segments = query_segments(query);
    if segments.is_empty() {
        // An empty query lists everything, which is how the launcher opens.
        // A query that merely folds away — Chinese characters, which the
        // launcher cannot type — must not.
        if !query.trim().is_empty() {
            return Vec::new();
        }
        return apps
            .iter()
            .enumerate()
            .filter(|(_, app)| wanted(app))
            .map(|(index, _)| (index, 0))
            .collect();
    }

    let mut rows = Rows::default();
    // (app index, score, key length used for the tie-break)
    let mut scored: Vec<(usize, u32, u32)> = Vec::new();

    if segments.len() == 1 {
        let segment = &segments[0];
        let mask = query_mask(segment);
        let threshold = threshold_for(segment.len());
        for (index, app) in apps.iter().enumerate() {
            if !wanted(app) {
                continue;
            }
            if let Some((score, key_len)) =
                best_key_score(segment, mask, &app.keys, threshold, &mut rows)
            {
                scored.push((index, score, key_len));
            }
        }
    } else {
        // Path query: each segment must match a successive component — the
        // ancestor folders in order, then the app itself — and an entry scores
        // as badly as its worst segment. Folder keys are only consulted here.
        for (index, app) in apps.iter().enumerate() {
            if !wanted(app) {
                continue;
            }
            let component_count = app.folder_keys.len() + 1;
            let mut cursor = 0usize;
            let mut worst = SCALE;
            let mut key_len = 0u32;
            let mut matched_all = true;

            for segment in &segments {
                let mask = query_mask(segment);
                let threshold = threshold_for(segment.len());
                let mut hit = None;
                for component in cursor..component_count {
                    let keys = if component + 1 == component_count {
                        &app.keys
                    } else {
                        &app.folder_keys[component]
                    };
                    if let Some((score, len)) =
                        best_key_score(segment, mask, keys, threshold, &mut rows)
                    {
                        hit = Some((component, score, len));
                        break;
                    }
                }
                let Some((component, score, len)) = hit else {
                    matched_all = false;
                    break;
                };
                worst = worst.min(score);
                key_len += len;
                cursor = component + 1;
            }

            if matched_all {
                scored.push((index, worst, key_len));
            }
        }
    }

    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| apps[a.0].display_name.cmp(&apps[b.0].display_name))
            .then_with(|| apps[a.0].path.cmp(&apps[b.0].path))
    });
    scored
        .into_iter()
        .map(|(index, score, _)| (index, score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::AppEntry;

    fn app(name: &str, path: &str, display: &str) -> AppEntry {
        AppEntry::with_display_name(name.into(), path.into(), Some(display.into()))
    }

    fn score(query: &str, entry: &AppEntry) -> Option<u32> {
        let apps = [entry.clone()];
        search_apps(query, &apps, false)
            .first()
            .map(|(_, score)| *score)
    }

    #[test]
    fn exact_and_prefix_queries_score_full_marks() {
        let safari = app("Safari", "/Applications/Safari.app", "Safari");
        assert_eq!(score("safari", &safari), Some(SCALE));
        assert_eq!(score("saf", &safari), Some(SCALE));
        assert_eq!(score("safa", &safari), Some(SCALE));
    }

    #[test]
    fn abbreviations_match() {
        let disk = app(
            "Disk Utility",
            "/Applications/Utilities/Disk Utility.app",
            "Disk Utility",
        );
        assert_eq!(score("du", &disk), Some(SCALE));

        let calculator = app(
            "Calculator",
            "/System/Applications/Calculator.app",
            "计算器",
        );
        assert_eq!(score("jsq", &calculator), Some(SCALE));
        assert_eq!(score("jisuanqi", &calculator), Some(SCALE));
    }

    #[test]
    fn missing_and_surplus_letters_still_match() {
        let wechat = app("WeChat", "/Applications/WeChat.app", "微信");
        // "wixin" drops a letter, "weixinn" adds one; both stay above the
        // threshold, and the extra letter costs confidence.
        let dropped = score("wixin", &wechat).expect("dropped letter must match");
        let exact = score("weixin", &wechat).expect("full pinyin must match");
        let surplus = score("weixinn", &wechat).expect("surplus letter must match");
        assert!(dropped > surplus, "{dropped} vs {surplus}");
        assert!(exact > surplus);

        let safari = app("Safari", "/Applications/Safari.app", "Safari");
        assert!(score("safarii", &safari).is_some(), "surplus letters");
        assert!(score("sai", &safari).is_some(), "dropped letters");
    }

    #[test]
    fn transpositions_are_tolerated_and_cost_confidence() {
        let gmail = app("Gmail", "/Applications/Gmail.app", "Gmail");
        let swapped = score("gmial", &gmail).expect("a swapped pair must match");
        assert!((600..SCALE).contains(&swapped), "{swapped}");

        // Several swapped pairs are handled too, and every pair costs
        // confidence: one pair scores above two, two above three.
        let letters = app("Letters", "/Applications/Letters.app", "abcdef");
        let exact = score("abcdef", &letters).expect("exact");
        let one = score("bacdef", &letters).expect("one swapped pair");
        let two = score("badcef", &letters).expect("two swapped pairs");
        let three = score("badcfe", &letters).unwrap_or(0);
        assert_eq!(exact, SCALE);
        assert!(
            one < exact && two < one && three < two,
            "{exact} {one} {two} {three}"
        );
        assert!(one >= 600 && two >= 600, "{one} {two}");
        // Three pairs inside a six-letter name are past the confidence floor:
        // not listing the entry is the intended outcome.
        assert!(three < 600, "{three}");
    }

    #[test]
    fn the_bundle_name_ranks_below_the_display_name() {
        let wechat = app("WeChat", "/Applications/WeChat.app", "微信");
        let display = score("weixin", &wechat).expect("pinyin of 微信");
        let bundle = score("wechat", &wechat).expect("bundle name");
        assert_eq!(display, SCALE);
        assert!(bundle < display, "{bundle} vs {display}");
        assert!(bundle >= 600);
    }

    #[test]
    fn a_single_character_only_matches_a_word_start() {
        let safari = app("Safari", "/Applications/Safari.app", "Safari");
        assert_eq!(score("s", &safari), Some(SCALE), "first letter");
        assert_eq!(score("a", &safari), None, "middle of a word");

        let disk = app(
            "Disk Utility",
            "/Applications/Utilities/Disk Utility.app",
            "Disk Utility",
        );
        assert_eq!(score("u", &disk), Some(SCALE), "second word");
    }

    #[test]
    fn unrelated_queries_are_dropped() {
        let wechat = app("WeChat", "/Applications/WeChat.app", "微信");
        assert_eq!(score("abc", &wechat), None);
        assert_eq!(score("zzzz", &wechat), None);
    }

    #[test]
    fn accents_are_folded() {
        let ecole = app("Ecole", "/Applications/Ecole.app", "école");
        for query in ["é", "É", "ÉCOLE", "ecole", "éco"] {
            assert!(score(query, &ecole).is_some(), "query {query}");
        }
    }

    #[test]
    fn prefixes_outrank_middle_matches() {
        let apps = [
            app("Unsafest", "/Applications/Unsafest.app", "Unsafest"),
            app("Safari", "/Applications/Safari.app", "Safari"),
        ];
        let hits = search_apps("saf", &apps, false);
        assert_eq!(hits.len(), 2, "both contain the letters");
        assert_eq!(hits[0].0, 1, "the prefix match must come first");
        assert!(hits[0].1 > hits[1].1);
    }

    #[test]
    fn folder_names_only_match_path_queries() {
        let app = app("My App", "/Applications/Dev Tools/My App.app", "My App");
        let single = std::slice::from_ref(&app);

        // A single segment must not reach an ancestor folder.
        let hits = search_apps("devtools", single, false);
        assert!(hits.is_empty(), "{hits:?}");

        // With a separator the folder is the point of the query.
        let hits = search_apps("devtools/myapp", single, false);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 0);

        // Order cannot be reversed, and abbreviations work per segment.
        assert!(search_apps("myapp/devtools", single, false).is_empty());
        assert_eq!(search_apps("devtools/ma", single, false).len(), 1);
        assert_eq!(search_apps("dev tools/mya", single, false).len(), 1);
        // Every segment has to reach its own threshold.
        assert!(search_apps("devtools/zz", single, false).is_empty());
    }

    #[test]
    fn an_empty_query_lists_every_app() {
        let apps = [
            app("Safari", "/Applications/Safari.app", "Safari"),
            app(
                "Calculator",
                "/System/Applications/Calculator.app",
                "计算器",
            ),
        ];
        let hits = search_apps("", &apps, false);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(_, score)| *score == 0));
    }

    #[test]
    fn the_apps_only_filter_skips_non_installed_bundles() {
        let installed = app("Foo", "/Applications/Foo.app", "Foo");
        let helper = app("Foo Helper", "/usr/libexec/Foo Helper.app", "Foo Helper");
        let apps = [installed, helper];

        assert_eq!(search_apps("foo", &apps, false).len(), 2);
        let filtered = search_apps("foo", &apps, true);
        assert_eq!(filtered.len(), 1, "{filtered:?}");
        assert_eq!(filtered[0].0, 0);
        // The empty query goes through the same filter.
        assert_eq!(search_apps("", &apps, true).len(), 1);
        // Path queries too.
        assert_eq!(search_apps("dev/foo", &apps, true).len(), 0);
    }

    #[test]
    fn a_thousand_apps_stay_responsive() {
        // Mirrors the real index size, so an accidental per-candidate
        // allocation or a missing pre-filter shows up as a slow test.
        let apps: Vec<AppEntry> = (0..1000)
            .map(|index| {
                app(
                    &format!("App{index:04}"),
                    &format!("/Applications/Suite {}/App{index:04}.app", index % 7),
                    &format!("应用{index:04}"),
                )
            })
            .collect();

        let started = std::time::Instant::now();
        for query in ["app", "app0420", "yingyong", "yy", "app042", "zzzz"] {
            let _ = search_apps(query, &apps, false);
        }
        let elapsed = started.elapsed();
        eprintln!("1000 apps x 6 queries: {elapsed:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "search must not become pathologically slow: {elapsed:?}"
        );

        // 应用0420 → pinyin "yingyong0420", abbreviation "yy0420"
        let hits = search_apps("yingyong0420", &apps, false);
        assert_eq!(hits[0].0, 420);
        let hits = search_apps("yy0420", &apps, false);
        assert_eq!(hits[0].0, 420);
    }
}
