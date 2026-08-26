use crate::apps::{AppEntry, PathComponent};

/// Fuzzy subsequence scorer. Higher is better; None means no match.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut qi = 0;
    let mut ti = 0;
    let mut score: i64 = 0;
    let mut prev_matched = false;

    while qi < q.len() && ti < t.len() {
        if q[qi].eq_ignore_ascii_case(&t[ti]) {
            if prev_matched {
                score += 10; // consecutive bonus
            }
            if ti == 0 {
                score += 100; // start-of-string bonus
            } else {
                let prev = t[ti - 1];
                match prev {
                    ' ' | '-' | '_' => score += 50,   // word boundary
                    _ if t[ti].is_uppercase() && !prev.is_uppercase() => score += 30,
                    _ => {}
                }
            }
            score += 1;
            prev_matched = true;
            qi += 1;
        } else {
            prev_matched = false;
        }
        ti += 1;
    }

    if qi == q.len() {
        Some(score - (ti as i64) / 2)
    } else {
        None
    }
}

/// Best fuzzy score across a candidate string list.
fn best_fuzzy(query: &str, candidates: &[String]) -> Option<i64> {
    candidates
        .iter()
        .filter_map(|c| fuzzy_score(query, c))
        .max()
}

/// Best fuzzy score of `seg` against one path component's name/pinyin fields.
fn match_component(seg: &str, c: &PathComponent) -> Option<i64> {
    best_fuzzy(seg, std::slice::from_ref(&c.name_lower))
        .or_else(|| best_fuzzy(seg, &c.pinyins))
        .or_else(|| best_fuzzy(seg, &c.initials))
}

/// Slash-separated hierarchical matching: each query segment must match a
/// successive (order-preserving) component of the app's location.
/// e.g. "shiyong/cipan" -> ["实用工具", "磁盘工具"].
fn match_hierarchical(segs: &[&str], components: &[PathComponent]) -> Option<i64> {
    let mut cursor = 0usize;
    let mut total: i64 = 0;

    for seg in segs {
        let mut best: Option<(usize, i64)> = None;
        for (i, c) in components.iter().enumerate().skip(cursor) {
            if let Some(s) = match_component(seg, c) {
                best = Some((i, s));
                break; // earliest match keeps the path order natural
            }
        }
        let (i, s) = best?;
        total += s + 60; // per-segment presence bonus
        cursor = i + 1;
    }

    Some(total)
}

pub fn search_apps<'a>(query: &str, apps: &'a [AppEntry]) -> Vec<(usize, i64)> {
    if query.is_empty() {
        return apps.iter().enumerate().map(|(i, _)| (i, 0)).collect();
    }

    let segments: Vec<&str> = query.split('/').map(str::trim).collect();

    let mut results: Vec<(usize, i64)> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, app)| {
            // Hierarchical path search takes over when the query contains '/'.
            if segments.len() > 1 {
                let score = match_hierarchical(&segments, &app.path_components)?;
                return Some((i, score + 400)); // explicit path intent ranks high
            }

            // Single-segment queries:
            let q_lower = segments[0];

            // 1) Localized display name (what the user sees) — highest priority.
            if let Some(score) = fuzzy_score(q_lower, &app.display_name_lower) {
                return Some((i, score + 1200));
            }
            // Pinyin variants of the localized display name
            // (polyphonic-aware: 音乐 -> yinle / yinyue).
            if let Some(score) = best_fuzzy(q_lower, &app.pinyins) {
                return Some((i, score + 200));
            }
            if let Some(score) = best_fuzzy(q_lower, &app.initials) {
                return Some((i, score - 200));
            }
            // 2) Original bundle stem as fallback (WeChat).
            if let Some(score) = fuzzy_score(q_lower, &app.name_lower) {
                return Some((i, score));
            }
            // 3) Any single ancestor folder ("shiyong" lists all 实用工具 apps).
            for comp in &app.path_components[..app.path_components.len().saturating_sub(1)] {
                if let Some(score) = match_component(q_lower, comp) {
                    return Some((i, score - 300));
                }
            }
            None
        })
        .collect();

    results.sort_by(|a, b| b.1.cmp(&a.1));
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::PathComponent;

    fn comp(name: &str) -> PathComponent {
        PathComponent::new(name.to_string())
    }

    #[test]
    fn hierarchical_slash_search() {
        // 实用工具/磁盘工具 scenario from the feature request.
        let components = vec![
            comp("实用工具".into()),  // pinyin: shiyonggongju / sygj
            comp("磁盘工具".into()),  // pinyin: cipangongju / cpgj
        ];
        assert!(match_hierarchical(&["shiyong", "cipan"], &components).is_some());
        assert!(match_hierarchical(&["sygj", "cpgj"], &components).is_some());
        assert!(match_hierarchical(&["实用", "磁盘"], &components).is_some());
        // Order matters: reversed segments must not match.
        assert!(match_hierarchical(&["cipan", "shiyong"], &components).is_none());
        // Missing second segment fails.
        assert!(match_hierarchical(&["shiyong", "nomatch"], &components).is_none());
    }

    #[test]
    fn polyphonic_pinyin_variants() {
        // 音乐: 乐 is polyphonic (le/yue) — "yinyue" must match.
        let (fulls, initials) = crate::apps::pinyin_variants("音乐");
        assert!(fulls.iter().any(|f| f == "yinyue"), "fulls: {fulls:?}");
        assert!(initials.iter().any(|i| i == "yy"), "initials: {initials:?}");

        let mut app = crate::apps::AppEntry::new("Music".into(), "/System/Applications/Music.app".into());
        app.display_name_lower = "音乐".into();
        let (pys, inits) = crate::apps::pinyin_variants("音乐");
        app.pinyins = pys;
        app.initials = inits;
        let apps = vec![app];

        for q in ["yinyue", "yy", "yinle"] {
            let hits = search_apps(q, &apps);
            assert_eq!(hits.len(), 1, "query {q} should hit 音乐");
        }
        // Non-matching query stays empty.
        assert!(search_apps("zzzz", &apps).is_empty());
    }

    #[test]
    fn search_apps_end_to_end() {
        let mut app = crate::apps::AppEntry::new("Disk Utility".into(), "/Applications/Utilities/Disk Utility.app".into());
        app.path_components = vec![comp("实用工具"), comp("磁盘工具")];
        app.in_app_dir = true;
        let apps = vec![app];

        let hits = search_apps("shiyong/cipan", &apps);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 0);

        let hits = search_apps("cipan/shiyong", &apps);
        assert!(hits.is_empty());
    }
}
