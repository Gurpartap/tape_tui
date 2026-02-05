//! Fuzzy matching utilities (Phase 13).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    let match_query = |normalized_query: &str| -> FuzzyMatch {
        if normalized_query.is_empty() {
            return FuzzyMatch {
                matches: true,
                score: 0.0,
            };
        }

        if normalized_query.len() > text_lower.len() {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        }

        let mut query_index = 0usize;
        let mut score = 0.0f64;
        let mut last_match_index: isize = -1;
        let mut consecutive_matches = 0i32;

        let text_chars: Vec<char> = text_lower.chars().collect();
        let query_chars: Vec<char> = normalized_query.chars().collect();

        for (i, ch) in text_chars.iter().enumerate() {
            if query_index >= query_chars.len() {
                break;
            }
            if *ch == query_chars[query_index] {
                let is_word_boundary = if i == 0 {
                    true
                } else {
                    let prev = text_chars[i - 1];
                    prev.is_whitespace() || matches!(prev, '-' | '_' | '.' | '/' | ':')
                };

                if last_match_index == i as isize - 1 {
                    consecutive_matches += 1;
                    score -= f64::from(consecutive_matches) * 5.0;
                } else {
                    consecutive_matches = 0;
                    if last_match_index >= 0 {
                        score += ((i as isize - last_match_index - 1) as f64) * 2.0;
                    }
                }

                if is_word_boundary {
                    score -= 10.0;
                }

                score += (i as f64) * 0.1;

                last_match_index = i as isize;
                query_index += 1;
            }
        }

        if query_index < query_chars.len() {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        }

        FuzzyMatch {
            matches: true,
            score,
        }
    };

    let primary_match = match_query(&query_lower);
    if primary_match.matches {
        return primary_match;
    }

    let swapped_query = if let Some((letters, digits)) = alpha_numeric_split(&query_lower) {
        format!("{digits}{letters}")
    } else if let Some((digits, letters)) = numeric_alpha_split(&query_lower) {
        format!("{letters}{digits}")
    } else {
        String::new()
    };

    if swapped_query.is_empty() {
        return primary_match;
    }

    let swapped_match = match_query(&swapped_query);
    if !swapped_match.matches {
        return primary_match;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

pub fn fuzzy_filter<T, F, S>(items: &[T], query: &str, get_text: F) -> Vec<T>
where
    T: Clone,
    F: Fn(&T) -> S,
    S: AsRef<str>,
{
    if query.trim().is_empty() {
        return items.to_vec();
    }

    let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return items.to_vec();
    }

    let mut results: Vec<(T, f64)> = Vec::new();

    for item in items {
        let text = get_text(item);
        let mut total_score = 0.0f64;
        let mut all_match = true;

        for token in &tokens {
            let matched = fuzzy_match(token, text.as_ref());
            if matched.matches {
                total_score += matched.score;
            } else {
                all_match = false;
                break;
            }
        }

        if all_match {
            results.push((item.clone(), total_score));
        }
    }

    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    results.into_iter().map(|(item, _)| item).collect()
}

fn alpha_numeric_split(input: &str) -> Option<(&str, &str)> {
    let mut idx = 0;
    let bytes = input.as_bytes();
    while idx < bytes.len() && bytes[idx].is_ascii_lowercase() {
        idx += 1;
    }
    if idx == 0 || idx == bytes.len() {
        return None;
    }
    if bytes[idx..].iter().all(|b| b.is_ascii_digit()) {
        Some((&input[..idx], &input[idx..]))
    } else {
        None
    }
}

fn numeric_alpha_split(input: &str) -> Option<(&str, &str)> {
    let mut idx = 0;
    let bytes = input.as_bytes();
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == 0 || idx == bytes.len() {
        return None;
    }
    if bytes[idx..].iter().all(|b| b.is_ascii_lowercase()) {
        Some((&input[..idx], &input[idx..]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_filter, fuzzy_match};

    #[test]
    fn fuzzy_match_prefers_consecutive_matches() {
        let direct = fuzzy_match("abc", "abc");
        let spaced = fuzzy_match("abc", "a_b_c");
        assert!(direct.matches);
        assert!(spaced.matches);
        assert!(direct.score < spaced.score);
    }

    #[test]
    fn fuzzy_match_swaps_alpha_numeric() {
        let swapped = fuzzy_match("ab12", "12ab");
        assert!(swapped.matches);
    }

    #[test]
    fn fuzzy_filter_requires_all_tokens() {
        let items = vec!["alpha beta", "alpha", "beta alpha"];
        let filtered = fuzzy_filter(&items, "alpha beta", |item| *item);
        assert_eq!(filtered, vec!["alpha beta", "beta alpha"]);
    }
}
