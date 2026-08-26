use crate::apps::AppEntry;

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

pub fn search_apps<'a>(query: &str, apps: &'a [AppEntry]) -> Vec<(usize, i64)> {
    if query.is_empty() {
        return apps.iter().enumerate().map(|(i, _)| (i, 0)).collect();
    }
    let q_lower = query.to_lowercase();

    let mut results: Vec<(usize, i64)> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, app)| {
            // 1) Localized display name (what the user sees) — highest priority.
            if let Some(score) = fuzzy_score(&q_lower, &app.display_name_lower) {
                return Some((i, score + 1200));
            }
            // Pinyin of the localized display name (微信 → weixin / wx).
            if let Some(score) = fuzzy_score(&q_lower, &app.pinyin_full) {
                return Some((i, score + 200));
            }
            if let Some(score) = fuzzy_score(&q_lower, &app.pinyin_initials) {
                return Some((i, score - 200));
            }
            // 2) Original bundle stem as fallback (WeChat).
            if let Some(score) = fuzzy_score(&q_lower, &app.name_lower) {
                return Some((i, score));
            }
            None
        })
        .collect();

    results.sort_by(|a, b| b.1.cmp(&a.1));
    results
}
