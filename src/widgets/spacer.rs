//! Spacer widget (Phase 8).

use crate::core::component::Component;

pub struct Spacer {
    lines: usize,
}

impl Spacer {
    pub fn new() -> Self {
        Self { lines: 1 }
    }

    pub fn with_lines(lines: usize) -> Self {
        Self { lines }
    }

    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines;
    }
}

impl Default for Spacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Spacer {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![String::new(); self.lines]
    }
}

#[cfg(test)]
mod tests {
    use super::Spacer;
    use crate::core::component::Component;

    #[test]
    fn spacer_renders_empty_lines() {
        let mut spacer = Spacer::with_lines(3);
        let lines = spacer.render(10);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.is_empty()));
    }

    #[test]
    fn spacer_default_is_one_line() {
        let mut spacer = Spacer::new();
        let lines = spacer.render(10);
        assert_eq!(lines.len(), 1);
    }
}
