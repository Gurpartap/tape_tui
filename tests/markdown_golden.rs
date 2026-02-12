mod fixture;

use tape_tui::core::component::Component;
use tape_tui::{Markdown, MarkdownTheme};

fn plain_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: Box::new(|text| text.to_string()),
        link: Box::new(|text| text.to_string()),
        link_url: Box::new(|text| text.to_string()),
        code: Box::new(|text| text.to_string()),
        code_block: Box::new(|text| text.to_string()),
        code_block_border: Box::new(|text| text.to_string()),
        quote: Box::new(|text| text.to_string()),
        quote_border: Box::new(|text| text.to_string()),
        hr: Box::new(|text| text.to_string()),
        list_bullet: Box::new(|text| text.to_string()),
        bold: Box::new(|text| text.to_string()),
        italic: Box::new(|text| text.to_string()),
        strikethrough: Box::new(|text| text.to_string()),
        underline: Box::new(|text| text.to_string()),
        highlight_code: None,
        code_block_indent: None,
    }
}

fn read_lines(name: &str) -> Vec<String> {
    let raw = fixture::read_fixture(name);
    let mut normalized = raw.replace("\r\n", "\n");
    if normalized.ends_with('\n') {
        normalized.pop();
        if normalized.ends_with('\r') {
            normalized.pop();
        }
    }
    if normalized.is_empty() {
        Vec::new()
    } else {
        normalized
            .split('\n')
            .map(|line| line.to_string())
            .collect()
    }
}

fn render_markdown(input_fixture: &str, width: usize) -> Vec<String> {
    let input = fixture::read_fixture(input_fixture);
    let mut markdown = Markdown::new(input, 0, 0, plain_theme(), None);
    markdown
        .render(width)
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect()
}

fn assert_markdown_fixture(input_fixture: &str, expected_fixture: &str, width: usize) {
    let actual = render_markdown(input_fixture, width);
    let expected = read_lines(expected_fixture);
    assert_eq!(
        actual, expected,
        "markdown golden mismatch for {input_fixture} at width={width}"
    );
}

#[test]
fn markdown_table_narrow_golden() {
    assert_markdown_fixture("markdown_table_narrow.md", "markdown_table_narrow.txt", 36);
}

#[test]
fn markdown_blockquote_wrap_golden() {
    assert_markdown_fixture(
        "markdown_blockquote_wrap.md",
        "markdown_blockquote_wrap.txt",
        26,
    );
}

#[test]
fn markdown_spacing_rules_golden() {
    assert_markdown_fixture(
        "markdown_spacing_rules.md",
        "markdown_spacing_rules.txt",
        32,
    );
}

#[test]
fn markdown_links_golden() {
    assert_markdown_fixture("markdown_links.md", "markdown_links.txt", 80);
}
