//! Markdown widget (Phase 22).

use crate::core::component::Component;
use crate::core::terminal_image::is_image_line;
use crate::core::text::slice::wrap_text_with_ansi;
use crate::core::text::utils::apply_background_to_line;
use crate::core::text::width::visible_width;

use markdown::{mdast, to_mdast, ParseOptions};

pub type MarkdownStyleFn = Box<dyn Fn(&str) -> String>;

pub type MarkdownCodeHighlighterFn = Box<dyn Fn(&str, Option<&str>) -> Vec<String>>;

pub struct DefaultTextStyle {
    pub color: Option<MarkdownStyleFn>,
    pub bg_color: Option<MarkdownStyleFn>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
}

pub struct MarkdownTheme {
    pub heading: MarkdownStyleFn,
    pub link: MarkdownStyleFn,
    pub link_url: MarkdownStyleFn,
    pub code: MarkdownStyleFn,
    pub code_block: MarkdownStyleFn,
    pub code_block_border: MarkdownStyleFn,
    pub quote: MarkdownStyleFn,
    pub quote_border: MarkdownStyleFn,
    pub hr: MarkdownStyleFn,
    pub list_bullet: MarkdownStyleFn,
    pub bold: MarkdownStyleFn,
    pub italic: MarkdownStyleFn,
    pub strikethrough: MarkdownStyleFn,
    pub underline: MarkdownStyleFn,
    pub highlight_code: Option<MarkdownCodeHighlighterFn>,
    pub code_block_indent: Option<String>,
}

#[derive(Clone, Copy)]
enum InlineStyleKind {
    Default,
    Quote,
}

struct InlineStyleContext {
    kind: InlineStyleKind,
    style_prefix: String,
}

pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    default_text_style: Option<DefaultTextStyle>,
    theme: MarkdownTheme,
    default_style_prefix: Option<String>,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl Markdown {
    pub fn new(
        text: impl Into<String>,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        default_text_style: Option<DefaultTextStyle>,
    ) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            default_text_style,
            theme,
            default_style_prefix: None,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.invalidate();
    }

    fn apply_default_style(&self, text: &str) -> String {
        let Some(style) = self.default_text_style.as_ref() else {
            return text.to_string();
        };

        let mut styled = text.to_string();
        if let Some(color) = style.color.as_ref() {
            styled = color(&styled);
        }
        if style.bold {
            styled = (self.theme.bold)(&styled);
        }
        if style.italic {
            styled = (self.theme.italic)(&styled);
        }
        if style.strikethrough {
            styled = (self.theme.strikethrough)(&styled);
        }
        if style.underline {
            styled = (self.theme.underline)(&styled);
        }
        styled
    }

    fn get_default_style_prefix(&mut self) -> String {
        if let Some(prefix) = self.default_style_prefix.as_ref() {
            return prefix.clone();
        }
        let Some(style) = self.default_text_style.as_ref() else {
            self.default_style_prefix = Some(String::new());
            return String::new();
        };

        let sentinel = "\u{0000}";
        let mut styled = sentinel.to_string();
        if let Some(color) = style.color.as_ref() {
            styled = color(&styled);
        }
        if style.bold {
            styled = (self.theme.bold)(&styled);
        }
        if style.italic {
            styled = (self.theme.italic)(&styled);
        }
        if style.strikethrough {
            styled = (self.theme.strikethrough)(&styled);
        }
        if style.underline {
            styled = (self.theme.underline)(&styled);
        }

        let prefix = styled
            .find(sentinel)
            .map(|idx| styled[..idx].to_string())
            .unwrap_or_default();
        self.default_style_prefix = Some(prefix.clone());
        prefix
    }

    fn get_style_prefix<F>(&self, style_fn: F) -> String
    where
        F: Fn(&str) -> String,
    {
        let sentinel = "\u{0000}";
        let styled = style_fn(sentinel);
        styled
            .find(sentinel)
            .map(|idx| styled[..idx].to_string())
            .unwrap_or_default()
    }

    fn apply_inline_style(&self, text: &str, kind: InlineStyleKind) -> String {
        match kind {
            InlineStyleKind::Default => self.apply_default_style(text),
            InlineStyleKind::Quote => (self.theme.quote)(&(self.theme.italic)(text)),
        }
    }

    fn apply_inline_style_with_newlines(&self, text: &str, kind: InlineStyleKind) -> String {
        text.split('\n')
            .map(|segment| self.apply_inline_style(segment, kind))
            .collect::<Vec<String>>()
            .join("\n")
    }

    fn default_inline_context(&mut self) -> InlineStyleContext {
        InlineStyleContext {
            kind: InlineStyleKind::Default,
            style_prefix: self.get_default_style_prefix(),
        }
    }

    fn render_inline_nodes(
        &mut self,
        nodes: &[mdast::Node],
        context: &InlineStyleContext,
    ) -> String {
        let style_prefix = context.style_prefix.as_str();
        let kind = context.kind;

        let mut result = String::new();

        for node in nodes {
            match node {
                mdast::Node::Text(text) => {
                    result.push_str(&self.apply_inline_style_with_newlines(&text.value, kind));
                }
                mdast::Node::Paragraph(paragraph) => {
                    let text = self.render_inline_nodes(&paragraph.children, context);
                    result.push_str(&text);
                }
                mdast::Node::Strong(strong) => {
                    let content = self.render_inline_nodes(&strong.children, context);
                    result.push_str(&(self.theme.bold)(&content));
                    result.push_str(style_prefix);
                }
                mdast::Node::Emphasis(emphasis) => {
                    let content = self.render_inline_nodes(&emphasis.children, context);
                    result.push_str(&(self.theme.italic)(&content));
                    result.push_str(style_prefix);
                }
                mdast::Node::Delete(delete) => {
                    let content = self.render_inline_nodes(&delete.children, context);
                    result.push_str(&(self.theme.strikethrough)(&content));
                    result.push_str(style_prefix);
                }
                mdast::Node::InlineCode(code) => {
                    result.push_str(&(self.theme.code)(&code.value));
                    result.push_str(style_prefix);
                }
                mdast::Node::Link(link) => {
                    let link_text = self.render_inline_nodes(&link.children, context);
                    let link_text_plain = plain_text_from_nodes(&link.children);
                    let href = link.url.as_str();
                    let href_cmp = href.strip_prefix("mailto:").unwrap_or(href);
                    if link_text_plain == href || link_text_plain == href_cmp {
                        let styled = (self.theme.link)(&(self.theme.underline)(&link_text));
                        result.push_str(&styled);
                    } else {
                        let styled = (self.theme.link)(&(self.theme.underline)(&link_text));
                        let url = (self.theme.link_url)(&format!(" ({href})"));
                        result.push_str(&styled);
                        result.push_str(&url);
                    }
                    result.push_str(style_prefix);
                }
                mdast::Node::Break(_) => {
                    result.push('\n');
                }
                mdast::Node::Html(html) => {
                    result.push_str(&self.apply_inline_style_with_newlines(&html.value, kind));
                }
                mdast::Node::Image(image) => {
                    let alt = if image.alt.is_empty() {
                        image.url.as_str()
                    } else {
                        image.alt.as_str()
                    };
                    result.push_str(&self.apply_inline_style_with_newlines(alt, kind));
                }
                mdast::Node::InlineMath(math) => {
                    result.push_str(&self.apply_inline_style_with_newlines(&math.value, kind));
                }
                mdast::Node::Math(math) => {
                    result.push_str(&self.apply_inline_style_with_newlines(&math.value, kind));
                }
                _ => {}
            }
        }

        result
    }

    fn render_list(&mut self, list: &mdast::List, depth: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let indent = "  ".repeat(depth);
        let start_number = list.start.unwrap_or(1);

        for (i, node) in list.children.iter().enumerate() {
            let mdast::Node::ListItem(item) = node else {
                continue;
            };
            let bullet = if list.ordered {
                format!("{}.", start_number + i as u32) + " "
            } else {
                "- ".to_string()
            };

            let item_lines = self.render_list_item(item, depth);
            if item_lines.is_empty() {
                lines.push(format!("{indent}{}", (self.theme.list_bullet)(&bullet)));
                continue;
            }

            let first_line = &item_lines[0];
            if is_nested_list_line(first_line) {
                lines.push(first_line.clone());
            } else {
                lines.push(format!(
                    "{indent}{}{}",
                    (self.theme.list_bullet)(&bullet),
                    first_line
                ));
            }

            for line in item_lines.iter().skip(1) {
                if is_nested_list_line(line) {
                    lines.push(line.clone());
                } else {
                    lines.push(format!("{indent}  {line}"));
                }
            }
        }

        lines
    }

    fn render_list_item(&mut self, item: &mdast::ListItem, depth: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let context = self.default_inline_context();

        for node in item.children.iter() {
            match node {
                mdast::Node::List(list) => {
                    lines.extend(self.render_list(list, depth + 1));
                }
                mdast::Node::Paragraph(paragraph) => {
                    let text = self.render_inline_nodes(&paragraph.children, &context);
                    lines.extend(text.split('\n').map(|line| line.to_string()));
                }
                mdast::Node::Text(text) => {
                    let text =
                        self.render_inline_nodes(&[mdast::Node::Text(text.clone())], &context);
                    lines.extend(text.split('\n').map(|line| line.to_string()));
                }
                mdast::Node::Code(code) => {
                    let indent = self
                        .theme
                        .code_block_indent
                        .clone()
                        .unwrap_or_else(|| "  ".to_string());
                    lines.push((self.theme.code_block_border)(&format!(
                        "```{}",
                        code.lang.clone().unwrap_or_default()
                    )));
                    if let Some(highlighter) = self.theme.highlight_code.as_ref() {
                        let highlighted = highlighter(&code.value, code.lang.as_deref());
                        for line in highlighted {
                            lines.push(format!("{indent}{line}"));
                        }
                    } else {
                        for line in code.value.split('\n') {
                            lines.push(format!("{indent}{}", (self.theme.code_block)(line)));
                        }
                    }
                    lines.push((self.theme.code_block_border)("```"));
                }
                mdast::Node::Html(html) => {
                    let text = self.render_inline_nodes(
                        &[mdast::Node::Html(mdast::Html {
                            value: html.value.clone(),
                            position: html.position.clone(),
                        })],
                        &context,
                    );
                    lines.extend(text.split('\n').map(|line| line.to_string()));
                }
                _ => {
                    let text = self.render_inline_nodes(std::slice::from_ref(node), &context);
                    if !text.is_empty() {
                        lines.extend(text.split('\n').map(|line| line.to_string()));
                    }
                }
            }
        }

        lines
    }

    fn render_blockquote(&mut self, blockquote: &mdast::Blockquote, width: usize) -> Vec<String> {
        let style_prefix =
            self.get_style_prefix(|text| (self.theme.quote)(&(self.theme.italic)(text)));
        let context = InlineStyleContext {
            kind: InlineStyleKind::Quote,
            style_prefix,
        };

        let quote_text = self.render_inline_nodes(&blockquote.children, &context);

        let mut lines = Vec::new();
        let quote_content_width = width.saturating_sub(2).max(1);
        for line in quote_text.split('\n') {
            let wrapped = wrap_text_with_ansi(line, quote_content_width);
            for wrapped_line in wrapped {
                lines.push(format!(
                    "{}{}",
                    (self.theme.quote_border)("│ "),
                    wrapped_line
                ));
            }
        }

        lines
    }

    fn get_longest_word_width(&self, text: &str, max_width: Option<usize>) -> usize {
        let mut longest = 0usize;
        for word in text.split_whitespace().filter(|word| !word.is_empty()) {
            longest = longest.max(visible_width(word));
        }
        if let Some(max_width) = max_width {
            longest.min(max_width)
        } else {
            longest
        }
    }

    fn wrap_cell_text(&mut self, text: &str, max_width: usize) -> Vec<String> {
        wrap_text_with_ansi(text, max_width.max(1))
    }

    fn render_table(
        &mut self,
        table: &mdast::Table,
        width: usize,
        raw: Option<&str>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let header_row = match table.children.first() {
            Some(mdast::Node::TableRow(row)) => row,
            _ => return lines,
        };
        let rows: Vec<&mdast::TableRow> = table
            .children
            .iter()
            .filter_map(|node| match node {
                mdast::Node::TableRow(row) => Some(row),
                _ => None,
            })
            .collect();

        let num_cols = header_row.children.len();
        if num_cols == 0 {
            return lines;
        }

        let border_overhead = 3 * num_cols + 1;
        let available_for_cells = width.saturating_sub(border_overhead);
        if available_for_cells < num_cols {
            if let Some(raw) = raw {
                let mut fallback = wrap_text_with_ansi(raw, width);
                fallback.push(String::new());
                return fallback;
            }
            return lines;
        }

        let max_unbroken_word_width = 30usize;

        let mut natural_widths = vec![0usize; num_cols];
        let mut min_word_widths = vec![1usize; num_cols];

        for (col_idx, cell) in header_row.children.iter().enumerate() {
            let cell_text = render_cell_text(self, cell);
            natural_widths[col_idx] = visible_width(&cell_text);
            min_word_widths[col_idx] = self
                .get_longest_word_width(&cell_text, Some(max_unbroken_word_width))
                .max(1);
        }

        for row in rows.iter().skip(1) {
            for (col_idx, cell) in row.children.iter().enumerate() {
                let cell_text = render_cell_text(self, cell);
                natural_widths[col_idx] = natural_widths[col_idx].max(visible_width(&cell_text));
                min_word_widths[col_idx] = min_word_widths[col_idx].max(
                    self.get_longest_word_width(&cell_text, Some(max_unbroken_word_width))
                        .max(1),
                );
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: usize = min_column_widths.iter().sum();

        if min_cells_width > available_for_cells {
            min_column_widths = vec![1usize; num_cols];
            let remaining = available_for_cells.saturating_sub(num_cols);

            if remaining > 0 {
                let total_weight: usize = min_word_widths
                    .iter()
                    .map(|width| width.saturating_sub(1))
                    .sum();

                let mut growth = vec![0usize; num_cols];
                for (idx, width) in min_word_widths.iter().enumerate() {
                    let weight = width.saturating_sub(1);
                    growth[idx] = if total_weight > 0 {
                        (weight * remaining) / total_weight
                    } else {
                        0
                    };
                    min_column_widths[idx] += growth[idx];
                }

                let allocated: usize = growth.iter().sum();
                let mut leftover = remaining.saturating_sub(allocated);
                for col_width in min_column_widths.iter_mut().take(num_cols) {
                    if leftover == 0 {
                        break;
                    }
                    *col_width += 1;
                    leftover -= 1;
                }
            }

            min_cells_width = min_column_widths.iter().sum();
        }

        let total_natural_width: usize = natural_widths.iter().sum::<usize>() + border_overhead;
        let column_widths = if total_natural_width <= width {
            natural_widths
                .iter()
                .zip(min_column_widths.iter())
                .map(|(natural, min)| (*natural).max(*min))
                .collect::<Vec<usize>>()
        } else {
            let total_grow_potential: usize = natural_widths
                .iter()
                .zip(min_column_widths.iter())
                .map(|(natural, min)| natural.saturating_sub(*min))
                .sum();
            let extra_width = available_for_cells.saturating_sub(min_cells_width);

            let mut widths = Vec::with_capacity(num_cols);
            for idx in 0..num_cols {
                let natural = natural_widths[idx];
                let min_width = min_column_widths[idx];
                let min_delta = natural.saturating_sub(min_width);
                let grow = if total_grow_potential > 0 {
                    (min_delta * extra_width) / total_grow_potential
                } else {
                    0
                };
                widths.push(min_width + grow);
            }

            let allocated: usize = widths.iter().sum();
            let mut remaining = available_for_cells.saturating_sub(allocated);
            while remaining > 0 {
                let mut grew = false;
                for idx in 0..num_cols {
                    if remaining == 0 {
                        break;
                    }
                    if widths[idx] < natural_widths[idx] {
                        widths[idx] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }

            widths
        };

        let top_border_cells: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        lines.push(format!("┌─{}─┐", top_border_cells.join("─┬─")));

        let mut header_lines: Vec<Vec<String>> = Vec::with_capacity(num_cols);
        for (idx, cell) in header_row.children.iter().enumerate() {
            let cell_text = render_cell_text(self, cell);
            header_lines.push(self.wrap_cell_text(&cell_text, column_widths[idx]));
        }
        let header_line_count = header_lines
            .iter()
            .map(|lines| lines.len())
            .max()
            .unwrap_or(0);

        for line_idx in 0..header_line_count {
            let mut row_parts = Vec::with_capacity(num_cols);
            for (col_idx, col_width) in column_widths.iter().enumerate().take(num_cols) {
                let text = header_lines
                    .get(col_idx)
                    .and_then(|lines| lines.get(line_idx))
                    .cloned()
                    .unwrap_or_default();
                let padding = col_width.saturating_sub(visible_width(&text));
                let padded = format!("{text}{}", " ".repeat(padding));
                row_parts.push((self.theme.bold)(&padded));
            }
            lines.push(format!("│ {} │", row_parts.join(" │ ")));
        }

        let separator_cells: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        let separator_line = format!("├─{}─┤", separator_cells.join("─┼─"));
        lines.push(separator_line.clone());

        for (row_index, row) in rows.iter().enumerate().skip(1) {
            let mut row_lines: Vec<Vec<String>> = Vec::with_capacity(num_cols);
            for (idx, cell) in row.children.iter().enumerate() {
                let cell_text = render_cell_text(self, cell);
                row_lines.push(self.wrap_cell_text(&cell_text, column_widths[idx]));
            }
            let row_line_count = row_lines.iter().map(|lines| lines.len()).max().unwrap_or(0);

            for line_idx in 0..row_line_count {
                let mut row_parts = Vec::with_capacity(num_cols);
                for (col_idx, col_width) in column_widths.iter().enumerate().take(num_cols) {
                    let text = row_lines
                        .get(col_idx)
                        .and_then(|lines| lines.get(line_idx))
                        .cloned()
                        .unwrap_or_default();
                    let padding = col_width.saturating_sub(visible_width(&text));
                    row_parts.push(format!("{text}{}", " ".repeat(padding)));
                }
                lines.push(format!("│ {} │", row_parts.join(" │ ")));
            }

            if row_index < rows.len() - 1 {
                lines.push(separator_line.clone());
            }
        }

        let bottom_border_cells: Vec<String> =
            column_widths.iter().map(|w| "─".repeat(*w)).collect();
        lines.push(format!("└─{}─┘", bottom_border_cells.join("─┴─")));
        lines.push(String::new());
        lines
    }

    fn render_node(
        &mut self,
        node: &mdast::Node,
        width: usize,
        next_is_list: bool,
        has_next: bool,
        space_after: bool,
        raw: Option<&str>,
    ) -> Vec<String> {
        match node {
            mdast::Node::Heading(heading) => {
                let context = self.default_inline_context();
                let heading_text = self.render_inline_nodes(&heading.children, &context);
                let styled = match heading.depth {
                    1 => (self.theme.heading)(&(self.theme.bold)(&(self.theme.underline)(
                        &heading_text,
                    ))),
                    2 => (self.theme.heading)(&(self.theme.bold)(&heading_text)),
                    _ => {
                        let prefix = "#".repeat(heading.depth as usize);
                        (self.theme.heading)(&(self.theme.bold)(&format!(
                            "{prefix} {heading_text}"
                        )))
                    }
                };
                let mut lines = vec![styled];
                if !space_after {
                    lines.push(String::new());
                }
                lines
            }
            mdast::Node::Paragraph(paragraph) => {
                let context = self.default_inline_context();
                let paragraph_text = self.render_inline_nodes(&paragraph.children, &context);
                let mut lines = vec![paragraph_text];
                if has_next && !next_is_list && !space_after {
                    lines.push(String::new());
                }
                lines
            }
            mdast::Node::Code(code) => {
                let indent = self
                    .theme
                    .code_block_indent
                    .clone()
                    .unwrap_or_else(|| "  ".to_string());
                let mut lines = Vec::new();
                lines.push((self.theme.code_block_border)(&format!(
                    "```{}",
                    code.lang.clone().unwrap_or_default()
                )));
                if let Some(highlighter) = self.theme.highlight_code.as_ref() {
                    let highlighted = highlighter(&code.value, code.lang.as_deref());
                    for line in highlighted {
                        lines.push(format!("{indent}{line}"));
                    }
                } else {
                    for line in code.value.split('\n') {
                        lines.push(format!("{indent}{}", (self.theme.code_block)(line)));
                    }
                }
                lines.push((self.theme.code_block_border)("```"));
                if !space_after {
                    lines.push(String::new());
                }
                lines
            }
            mdast::Node::List(list) => self.render_list(list, 0),
            mdast::Node::Blockquote(blockquote) => {
                let mut lines = self.render_blockquote(blockquote, width);
                if !space_after {
                    lines.push(String::new());
                }
                lines
            }
            mdast::Node::ThematicBreak(_) => {
                let hr_text = "─".repeat(width.min(80));
                let mut lines = vec![(self.theme.hr)(&hr_text)];
                if !space_after {
                    lines.push(String::new());
                }
                lines
            }
            mdast::Node::Html(html) => {
                vec![self.apply_default_style(html.value.trim())]
            }
            mdast::Node::Table(table) => self.render_table(table, width, raw),
            mdast::Node::Text(text) => vec![self.apply_default_style(&text.value)],
            mdast::Node::Break(_) => vec![String::new()],
            _ => Vec::new(),
        }
    }
}

impl Component for Markdown {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some(cached) = self.cached_lines.as_ref() {
            if self.cached_text.as_deref() == Some(self.text.as_str())
                && self.cached_width == Some(width)
            {
                return cached.clone();
            }
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);

        if self.text.trim().is_empty() {
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(Vec::new());
            return Vec::new();
        }

        let normalized_text = self.text.replace('\t', "   ");
        let root = match to_mdast(&normalized_text, &ParseOptions::gfm()) {
            Ok(node) => node,
            Err(_) => mdast::Node::Text(mdast::Text {
                value: normalized_text.clone(),
                position: None,
            }),
        };

        let nodes = match root {
            mdast::Node::Root(root) => root.children,
            other => vec![other],
        };

        let mut rendered_lines = Vec::new();
        for idx in 0..nodes.len() {
            let node = &nodes[idx];
            let next_node = nodes.get(idx + 1);
            let next_is_list = matches!(next_node, Some(mdast::Node::List(_)));
            let has_next = next_node.is_some();

            let space_after = match (node_position(node), next_node.and_then(node_position)) {
                (Some((end, _)), Some((_, next_start))) => {
                    has_blank_line_between(&normalized_text, end, next_start)
                }
                _ => false,
            };

            let raw = raw_slice_between(node, &normalized_text);
            let mut lines = self.render_node(
                node,
                content_width,
                next_is_list,
                has_next,
                space_after,
                raw.as_deref(),
            );
            rendered_lines.append(&mut lines);

            if space_after {
                rendered_lines.push(String::new());
            }
        }

        let mut wrapped_lines = Vec::new();
        for line in rendered_lines {
            if is_image_line(&line) {
                wrapped_lines.push(line);
            } else {
                wrapped_lines.extend(wrap_text_with_ansi(&line, content_width));
            }
        }

        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let bg_fn = self
            .default_text_style
            .as_ref()
            .and_then(|style| style.bg_color.as_ref());

        let mut content_lines = Vec::new();
        for line in wrapped_lines {
            if is_image_line(&line) {
                content_lines.push(line);
                continue;
            }

            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            if let Some(bg_fn) = bg_fn {
                content_lines.push(apply_background_to_line(&line_with_margins, width, bg_fn));
            } else {
                let visible_len = visible_width(&line_with_margins);
                let padding_needed = width.saturating_sub(visible_len);
                content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
            }
        }

        let empty_line = " ".repeat(width);
        let mut empty_lines = Vec::new();
        for _ in 0..self.padding_y {
            if let Some(bg_fn) = bg_fn {
                empty_lines.push(apply_background_to_line(&empty_line, width, bg_fn));
            } else {
                empty_lines.push(empty_line.clone());
            }
        }

        let mut result = Vec::new();
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines.iter().cloned());

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(result.clone());

        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }

    fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }
}

fn plain_text_from_nodes(nodes: &[mdast::Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            mdast::Node::Text(text) => out.push_str(&text.value),
            mdast::Node::InlineCode(code) => out.push_str(&code.value),
            mdast::Node::Strong(strong) => out.push_str(&plain_text_from_nodes(&strong.children)),
            mdast::Node::Emphasis(emphasis) => {
                out.push_str(&plain_text_from_nodes(&emphasis.children))
            }
            mdast::Node::Delete(delete) => out.push_str(&plain_text_from_nodes(&delete.children)),
            mdast::Node::Link(link) => out.push_str(&plain_text_from_nodes(&link.children)),
            mdast::Node::Html(html) => out.push_str(&html.value),
            mdast::Node::Image(image) => out.push_str(&image.alt),
            mdast::Node::Paragraph(paragraph) => {
                out.push_str(&plain_text_from_nodes(&paragraph.children))
            }
            _ => {}
        }
    }
    out
}

fn render_cell_text(widget: &mut Markdown, cell: &mdast::Node) -> String {
    match cell {
        mdast::Node::TableCell(table_cell) => {
            let context = widget.default_inline_context();
            widget.render_inline_nodes(&table_cell.children, &context)
        }
        _ => {
            let context = widget.default_inline_context();
            widget.render_inline_nodes(std::slice::from_ref(cell), &context)
        }
    }
}

fn node_position(node: &mdast::Node) -> Option<(usize, usize)> {
    let position = match node {
        mdast::Node::Heading(heading) => heading.position.as_ref(),
        mdast::Node::Paragraph(paragraph) => paragraph.position.as_ref(),
        mdast::Node::Code(code) => code.position.as_ref(),
        mdast::Node::List(list) => list.position.as_ref(),
        mdast::Node::Blockquote(blockquote) => blockquote.position.as_ref(),
        mdast::Node::ThematicBreak(thematic) => thematic.position.as_ref(),
        mdast::Node::Html(html) => html.position.as_ref(),
        mdast::Node::Table(table) => table.position.as_ref(),
        mdast::Node::Text(text) => text.position.as_ref(),
        _ => None,
    };
    position.map(|pos| (pos.end.offset, pos.start.offset))
}

fn raw_slice_between(node: &mdast::Node, source: &str) -> Option<String> {
    let position = match node {
        mdast::Node::Table(table) => table.position.as_ref(),
        _ => None,
    }?;

    let start = position.start.offset.min(source.len());
    let end = position.end.offset.min(source.len());
    if start >= end {
        return None;
    }
    Some(source[start..end].to_string())
}

fn has_blank_line_between(source: &str, end: usize, start: usize) -> bool {
    if start <= end || end >= source.len() {
        return false;
    }
    let slice_end = start.min(source.len());
    let slice = &source[end..slice_end];
    let mut saw_newline = false;
    let mut only_whitespace = true;

    for ch in slice.chars() {
        if ch == '\n' || ch == '\r' {
            if saw_newline && only_whitespace {
                return true;
            }
            saw_newline = true;
            only_whitespace = true;
        } else if ch.is_whitespace() {
            if saw_newline {
                continue;
            }
        } else {
            saw_newline = false;
            only_whitespace = false;
        }
    }

    false
}

fn is_nested_list_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("\x1b[36m") {
        if let Some(ch) = rest.chars().next() {
            return ch == '-' || ch.is_ascii_digit();
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{DefaultTextStyle, Markdown, MarkdownTheme};
    use crate::core::component::Component;

    fn theme() -> MarkdownTheme {
        MarkdownTheme {
            heading: Box::new(|text| format!("<h>{text}</h>")),
            link: Box::new(|text| format!("<l>{text}</l>")),
            link_url: Box::new(|text| format!("<u>{text}</u>")),
            code: Box::new(|text| format!("`{text}`")),
            code_block: Box::new(|text| format!("<code>{text}</code>")),
            code_block_border: Box::new(|text| format!("<cb>{text}</cb>")),
            quote: Box::new(|text| format!("<q>{text}</q>")),
            quote_border: Box::new(|text| text.to_string()),
            hr: Box::new(|text| format!("<hr>{text}</hr>")),
            list_bullet: Box::new(|text| format!("<b>{text}</b>")),
            bold: Box::new(|text| format!("<b>{text}</b>")),
            italic: Box::new(|text| format!("<i>{text}</i>")),
            strikethrough: Box::new(|text| format!("<s>{text}</s>")),
            underline: Box::new(|text| format!("<u>{text}</u>")),
            highlight_code: None,
            code_block_indent: None,
        }
    }

    #[test]
    fn headings_apply_styles_and_spacing() {
        let mut markdown = Markdown::new("# Title\nParagraph", 0, 0, theme(), None);
        let lines = markdown.render(40);
        assert_eq!(lines[0].trim_end(), "<h><b><u>Title</u></b></h>");
        assert_eq!(lines[1].trim_end(), "");
        assert_eq!(lines[2].trim_end(), "Paragraph");
    }

    #[test]
    fn link_renders_url_only_when_needed() {
        let mut markdown = Markdown::new("[x](x)\n[y](z)", 0, 0, theme(), None);
        let lines = markdown.render(80);
        assert_eq!(lines[0].trim_end(), "<l><u>x</u></l>");
        assert_eq!(lines[1].trim_end(), "<l><u>y</u></l><u> (z)</u>");
    }

    #[test]
    fn html_tokens_render_raw() {
        let mut markdown = Markdown::new("<span>hi</span>", 0, 0, theme(), None);
        let lines = markdown.render(80);
        assert_eq!(lines[0].trim_end(), "<span>hi</span>");
    }

    #[test]
    fn blockquote_wraps_and_prefixes() {
        let mut markdown = Markdown::new("> quote", 0, 0, theme(), None);
        let lines = markdown.render(80);
        assert_eq!(lines[0].trim_end(), "│ <q><i>quote</i></q>");
    }

    #[test]
    fn list_renders_bullets() {
        let mut markdown = Markdown::new("- one\n- two", 0, 0, theme(), None);
        let lines = markdown.render(80);
        assert!(lines[0].contains("<b>- </b>one"));
        assert!(lines[1].contains("<b>- </b>two"));
    }

    #[test]
    fn table_renders_borders() {
        let input = "| a | b |\n| - | - |\n| c | d |";
        let mut markdown = Markdown::new(input, 0, 0, theme(), None);
        let lines = markdown.render(80);
        assert!(lines.iter().any(|line| line.starts_with("┌")));
        assert!(lines.iter().any(|line| line.starts_with("└")));
    }

    #[test]
    fn default_style_applies_prefix() {
        let style = DefaultTextStyle {
            color: Some(Box::new(|text| format!("<c>{text}</c>"))),
            bg_color: None,
            bold: false,
            italic: false,
            strikethrough: false,
            underline: false,
        };
        let mut markdown = Markdown::new("hello **world**", 0, 0, theme(), Some(style));
        let lines = markdown.render(80);
        assert!(lines[0].starts_with("<c>hello </c><b><c>world</c></b>"));
    }
}
