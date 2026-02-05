mod fixture;

use pi_tui::core::terminal::Terminal;
use pi_tui::core::terminal_image::is_image_line;
use pi_tui::render::renderer::DiffRenderer;

#[derive(Default)]
struct TestTerminal {
    output: String,
    columns: u16,
    rows: u16,
}

impl TestTerminal {
    fn new(columns: u16, rows: u16) -> Self {
        Self {
            output: String::new(),
            columns,
            rows,
        }
    }

    fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }
}

impl Terminal for TestTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(String) + Send>, _on_resize: Box<dyn FnMut() + Send>) {}
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, data: &str) {
        self.output.push_str(data);
    }
    fn columns(&self) -> u16 {
        self.columns
    }
    fn rows(&self) -> u16 {
        self.rows
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: i32) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
}

fn not_image(_: &str) -> bool {
    false
}

#[test]
fn golden_first_render() {
    let expected = fixture::read_unescaped("renderer_first_render.txt");
    let mut renderer = DiffRenderer::new();
    let mut term = TestTerminal::new(10, 5);
    renderer.render(&mut term, vec!["hello".to_string()], not_image, false, false);
    let output = term.take_output();
    assert_eq!(output, expected);
}

#[test]
fn golden_width_change_full_clear() {
    let expected = fixture::read_unescaped("renderer_width_change_clear.txt");
    let mut renderer = DiffRenderer::new();
    let mut term = TestTerminal::new(10, 5);
    renderer.render(&mut term, vec!["hello".to_string()], not_image, false, false);
    term.take_output();

    term.columns = 12;
    renderer.render(&mut term, vec!["hello".to_string()], not_image, false, false);
    let output = term.take_output();
    assert_eq!(output, expected);
}

#[test]
fn golden_diff_one_line() {
    let expected = fixture::read_unescaped("renderer_diff_one_line.txt");
    let mut renderer = DiffRenderer::new();
    let mut term = TestTerminal::new(20, 5);
    renderer.render(
        &mut term,
        vec!["one".to_string(), "two".to_string()],
        not_image,
        false,
        false,
    );
    term.take_output();

    renderer.render(
        &mut term,
        vec!["one".to_string(), "tWO".to_string()],
        not_image,
        false,
        false,
    );
    let output = term.take_output();
    assert_eq!(output, expected);
}

#[test]
fn golden_clear_on_shrink() {
    let expected = fixture::read_unescaped("renderer_clear_on_shrink.txt");
    let mut renderer = DiffRenderer::new();
    let mut term = TestTerminal::new(20, 5);
    renderer.render(
        &mut term,
        vec!["one".to_string(), "two".to_string()],
        not_image,
        true,
        false,
    );
    term.take_output();

    renderer.render(&mut term, vec!["one".to_string()], not_image, true, false);
    let output = term.take_output();
    assert_eq!(output, expected);
}

#[test]
fn golden_image_line_bypass() {
    let expected = fixture::read_unescaped("renderer_image_line.txt");
    let mut renderer = DiffRenderer::new();
    let mut term = TestTerminal::new(5, 5);
    renderer.render(&mut term, vec!["short".to_string()], is_image_line, false, false);
    term.take_output();

    renderer.render(
        &mut term,
        vec!["\x1b_G1234567890".to_string()],
        is_image_line,
        false,
        false,
    );
    let output = term.take_output();
    assert_eq!(output, expected);
}
