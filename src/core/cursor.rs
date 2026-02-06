use crate::core::text::width::visible_width;

pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub row: usize,
    pub col: usize,
}

pub(crate) fn extract_cursor_marker(lines: &mut [String], height: usize) -> Option<CursorPos> {
    if lines.is_empty() {
        return None;
    }
    let viewport_top = lines.len().saturating_sub(height);
    for row in (viewport_top..lines.len()).rev() {
        let line = &lines[row];
        if let Some(index) = line.find(CURSOR_MARKER) {
            let before = &line[..index];
            let col = visible_width(before);
            let marker_end = index + CURSOR_MARKER.len();
            let mut updated = String::with_capacity(line.len().saturating_sub(CURSOR_MARKER.len()));
            updated.push_str(&line[..index]);
            updated.push_str(&line[marker_end..]);
            lines[row] = updated;
            return Some(CursorPos { row, col });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{extract_cursor_marker, CursorPos, CURSOR_MARKER};

    #[test]
    fn extracts_cursor_marker_and_removes_it() {
        let mut lines = vec![format!("hello{CURSOR_MARKER}")];
        let pos = extract_cursor_marker(&mut lines, 10);
        assert_eq!(pos, Some(CursorPos { row: 0, col: 5 }));
        assert_eq!(lines[0], "hello");
    }

    #[test]
    fn extraction_is_viewport_aware() {
        let mut lines = vec![
            format!("top{CURSOR_MARKER}"),
            "mid".to_string(),
            "bot".to_string(),
        ];
        let pos = extract_cursor_marker(&mut lines, 2);
        assert_eq!(pos, None);
        assert_eq!(lines[0], format!("top{CURSOR_MARKER}"));
    }
}
