mod fixture;

use pi_tui::core::input::{matches_key, parse_key, set_kitty_protocol_active};

#[test]
fn key_vectors_match_fixture() {
    let raw = fixture::read_fixture("key_vectors.tsv");
    for (idx, line) in raw.lines().enumerate() {
        let line_num = idx + 1;
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        let kind = parts.first().copied().unwrap_or("");
        match kind {
            "parse" => {
                assert!(
                    parts.len() == 4,
                    "line {line_num}: expected 4 columns for parse, got {}",
                    parts.len()
                );
                let kitty = parts[1].trim();
                let input = fixture::unescape(parts[2]);
                let expected_raw = parts[3].trim();
                set_kitty_protocol_active(kitty == "1");
                let expected = if expected_raw == "none" {
                    None
                } else {
                    Some(fixture::unescape(expected_raw))
                };
                let actual = parse_key(&input);
                assert_eq!(
                    actual, expected,
                    "line {line_num}: parse_key({input:?}) mismatch"
                );
            }
            "match" => {
                assert!(
                    parts.len() == 5,
                    "line {line_num}: expected 5 columns for match, got {}",
                    parts.len()
                );
                let kitty = parts[1].trim();
                let input = fixture::unescape(parts[2]);
                let key_id = fixture::unescape(parts[3]);
                let expected = match parts[4].trim() {
                    "true" => true,
                    "false" => false,
                    other => panic!("line {line_num}: invalid expected value {other}"),
                };
                set_kitty_protocol_active(kitty == "1");
                let actual = matches_key(&input, &key_id);
                assert_eq!(
                    actual, expected,
                    "line {line_num}: matches_key({input:?}, {key_id:?}) mismatch"
                );
            }
            other => panic!("line {line_num}: unknown kind {other}"),
        }
    }
    set_kitty_protocol_active(false);
}
