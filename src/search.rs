use crate::apps::AppEntry;

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
            // consecutive bonus
            if prev_matched {
                score += 10;
            }
            // beginning of string bonus
            if ti == 0 {
                score += 100;
            }
            // word boundary bonus (after space/hyphen/capital)
            if ti > 0 {
                let prev = t[ti - 1];
                if prev == ' ' || prev == '-' || prev == '_' {
                    score += 50;
                } else if t[ti].is_uppercase() && !t[ti - 1].is_uppercase() {
                    score += 30;
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
        // penalty for total distance
        score -= (ti as i64) / 2;
        Some(score)
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
            // 1) fuzzy match on original name
            if let Some(score) = fuzzy_score(&q_lower, &app.name_lower) {
                return Some((i, score + 1000)); // name match gets priority
            }
            // 2) pinyin full match
            if let Some(score) = fuzzy_score(&q_lower, &app.pinyin_full) {
                return Some((i, score));
            }
            // 3) pinyin initials match
            if let Some(score) = fuzzy_score(&q_lower, &app.pinyin_initials) {
                return Some((i, score - 100)); // lower priority
            }
            None
        })
        .collect();

    results.sort_by(|a, b| b.1.cmp(&a.1));
    results
}
