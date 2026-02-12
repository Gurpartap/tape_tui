//! Inline viewport state helper.
//!
//! Keeps viewport anchoring and scroll-offset clamping deterministic for runtime-owned
//! inline rendering semantics.

use crate::core::cursor::CursorPos;

#[derive(Debug, Clone, Copy)]
pub(crate) struct InlineViewportState {
    total_lines: usize,
    terminal_height: usize,
    scroll_offset_from_tail: usize,
}

impl Default for InlineViewportState {
    fn default() -> Self {
        Self {
            total_lines: 0,
            terminal_height: 0,
            scroll_offset_from_tail: 0,
        }
    }
}

impl InlineViewportState {
    pub(crate) fn note_terminal_height(&mut self, height: usize) {
        self.terminal_height = height;
        self.clamp_scroll_offset();
    }

    pub(crate) fn update_total_lines(&mut self, total_lines: usize) {
        self.total_lines = total_lines;
        self.clamp_scroll_offset();
    }

    pub(crate) fn viewport_top(&self) -> usize {
        let height = self.terminal_height.max(1);
        let max_top = self.total_lines.saturating_sub(height);
        max_top.saturating_sub(self.scroll_offset_from_tail.min(max_top))
    }

    pub(crate) fn clamp_cursor(&self, cursor: Option<CursorPos>) -> Option<CursorPos> {
        let Some(pos) = cursor else {
            return None;
        };
        let viewport_top = self.viewport_top();
        if pos.row < viewport_top || pos.row >= self.total_lines {
            return None;
        }
        Some(pos)
    }

    #[cfg(test)]
    pub(crate) fn set_scroll_offset_from_tail(&mut self, offset: usize) {
        self.scroll_offset_from_tail = offset;
        self.clamp_scroll_offset();
    }

    #[cfg(test)]
    pub(crate) fn scroll_offset_from_tail(&self) -> usize {
        self.scroll_offset_from_tail
    }

    #[cfg(test)]
    pub(crate) fn is_following_tail(&self) -> bool {
        self.scroll_offset_from_tail == 0
    }

    fn clamp_scroll_offset(&mut self) {
        let height = self.terminal_height.max(1);
        let max_offset = self.total_lines.saturating_sub(height);
        self.scroll_offset_from_tail = self.scroll_offset_from_tail.min(max_offset);
    }
}

#[cfg(test)]
mod tests {
    use super::InlineViewportState;
    use crate::core::cursor::CursorPos;

    #[test]
    fn follow_tail_anchor_tracks_latest_lines() {
        let mut state = InlineViewportState::default();
        state.note_terminal_height(4);
        state.update_total_lines(10);

        assert!(state.is_following_tail());
        assert_eq!(state.viewport_top(), 6);
    }

    #[test]
    fn scroll_offset_clamps_when_resize_reduces_available_history() {
        let mut state = InlineViewportState::default();
        state.note_terminal_height(5);
        state.update_total_lines(20);
        state.set_scroll_offset_from_tail(6);

        assert_eq!(state.viewport_top(), 9);
        assert_eq!(state.scroll_offset_from_tail(), 6);

        state.note_terminal_height(18);

        assert_eq!(state.scroll_offset_from_tail(), 2);
        assert_eq!(state.viewport_top(), 0);
    }

    #[test]
    fn cursor_clamp_respects_current_viewport_window() {
        let mut state = InlineViewportState::default();
        state.note_terminal_height(4);
        state.update_total_lines(10);

        assert_eq!(state.clamp_cursor(Some(CursorPos { row: 5, col: 0 })), None);
        assert_eq!(
            state.clamp_cursor(Some(CursorPos { row: 7, col: 0 })),
            Some(CursorPos { row: 7, col: 0 })
        );

        state.set_scroll_offset_from_tail(2);
        assert_eq!(state.viewport_top(), 4);
        assert_eq!(
            state.clamp_cursor(Some(CursorPos { row: 5, col: 0 })),
            Some(CursorPos { row: 5, col: 0 })
        );
    }
}
