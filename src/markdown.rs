use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{easy::HighlightLines, highlighting::Theme, parsing::SyntaxSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Rgb(76, 207, 176);
const MUTED: Color = Color::Rgb(113, 132, 160);
const CODE_BG: Color = Color::Rgb(23, 31, 45);

/// Parse CommonMark/GFM into styled terminal lines. No source text is modified.
pub fn render(
    source: &str,
    width: usize,
    syntaxes: &SyntaxSet,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut builder = Builder::default();
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                builder.blank();
                builder.push_style(Style::default().fg(ACCENT).add_modifier(
                    Modifier::BOLD
                        | if (level as usize) <= 2 {
                            Modifier::UNDERLINED
                        } else {
                            Modifier::empty()
                        },
                ));
            }
            Event::End(TagEnd::Heading(_)) => {
                builder.flush();
                builder.pop_style();
                builder.blank();
            }
            Event::Start(Tag::Paragraph) => {
                if builder.lists.is_empty() {
                    builder.flush();
                }
            }
            Event::End(TagEnd::Paragraph) => {
                builder.flush();
                if builder.lists.is_empty() {
                    builder.blank();
                }
            }
            Event::Start(Tag::Strong) => {
                builder.push_style(Style::default().add_modifier(Modifier::BOLD))
            }
            Event::Start(Tag::Emphasis) => {
                builder.push_style(Style::default().add_modifier(Modifier::ITALIC))
            }
            Event::Start(Tag::Strikethrough) => {
                builder.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                builder.pop_style()
            }
            Event::Start(Tag::BlockQuote(_)) => {
                builder.flush();
                builder.quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                builder.flush();
                builder.quote = builder.quote.saturating_sub(1);
                builder.blank();
            }
            Event::Start(Tag::List(start)) => {
                builder.flush();
                builder.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                builder.flush();
                builder.lists.pop();
                if builder.lists.is_empty() {
                    builder.blank();
                }
            }
            Event::Start(Tag::Item) => {
                builder.flush();
                let marker = match builder.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number = number.saturating_add(1);
                        marker
                    }
                    _ => "• ".into(),
                };
                builder.text(&marker, Style::default().fg(ACCENT));
            }
            Event::End(TagEnd::Item) => builder.flush(),
            Event::TaskListMarker(checked) => builder.text(
                if checked { "☑ " } else { "☐ " },
                Style::default().fg(ACCENT),
            ),
            Event::Start(Tag::CodeBlock(kind)) => {
                builder.blank();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                builder.code = Some((language, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((language, code)) = builder.code.take() {
                    builder.text(
                        &format!(
                            "┌─ {language}{}",
                            if language.eq_ignore_ascii_case("mermaid") {
                                "  [F9]"
                            } else {
                                ""
                            }
                        ),
                        Style::default().fg(MUTED).bg(CODE_BG),
                    );
                    builder.flush();
                    let syntax = syntaxes
                        .find_syntax_by_token(&language)
                        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
                    let mut highlighter = HighlightLines::new(syntax, theme);
                    for line in code.split_inclusive('\n') {
                        builder.text("│ ", Style::default().fg(MUTED).bg(CODE_BG));
                        match highlighter.highlight_line(line, syntaxes) {
                            Ok(ranges) => {
                                for (style, content) in ranges {
                                    builder.text(
                                        content.trim_end_matches(['\r', '\n']),
                                        Style::default()
                                            .fg(Color::Rgb(
                                                style.foreground.r,
                                                style.foreground.g,
                                                style.foreground.b,
                                            ))
                                            .bg(CODE_BG),
                                    );
                                }
                            }
                            Err(_) => builder.text(
                                line.trim_end_matches(['\r', '\n']),
                                Style::default().bg(CODE_BG),
                            ),
                        }
                        builder.flush();
                    }
                    builder.text("└─", Style::default().fg(MUTED));
                    builder.flush();
                    builder.blank();
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                builder.links.push((dest_url.into_string(), false));
                builder.push_style(
                    Style::default()
                        .fg(Color::Rgb(126, 182, 255))
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                builder.links.push((dest_url.into_string(), true));
                builder.text("▧ ", Style::default().fg(MUTED));
                builder.push_style(Style::default().fg(MUTED));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                builder.pop_style();
                if let Some((url, _)) = builder.links.pop() {
                    builder.text(&format!(" ({url})"), Style::default().fg(MUTED));
                }
            }
            Event::Start(Tag::Table(_)) => {
                builder.blank();
                builder.table = Some(vec![]);
            }
            Event::Start(Tag::TableHead) => {
                builder.cells.clear();
                builder.push_style(Style::default().add_modifier(Modifier::BOLD));
            }
            Event::Start(Tag::TableRow) => builder.cells.clear(),
            Event::Start(Tag::TableCell) => builder.pending.clear(),
            Event::End(TagEnd::TableCell) => builder
                .cells
                .push(Line::from(std::mem::take(&mut builder.pending))),
            Event::End(TagEnd::TableRow | TagEnd::TableHead) => {
                if let Some(table) = builder.table.as_mut() {
                    table.push(std::mem::take(&mut builder.cells));
                }
                if matches!(event, Event::End(TagEnd::TableHead)) {
                    builder.pop_style();
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = builder.table.take() {
                    builder.table_lines(table, width);
                }
                builder.blank();
            }
            Event::Text(text) => {
                if let Some((_, code)) = builder.code.as_mut() {
                    code.push_str(&text);
                } else {
                    builder.text(&text, builder.style());
                }
            }
            Event::Code(text) => builder.text(
                &text,
                builder.style().fg(Color::Rgb(235, 193, 119)).bg(CODE_BG),
            ),
            Event::SoftBreak => builder.text(" ", builder.style()),
            Event::HardBreak => builder.flush(),
            Event::Rule => {
                builder.blank();
                builder.text(&"─".repeat(width.min(80)), Style::default().fg(MUTED));
                builder.flush();
                builder.blank();
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                for line in text.lines() {
                    builder.text(line, Style::default().fg(MUTED));
                    if text.contains('\n') {
                        builder.flush();
                    }
                }
            }
            _ => {}
        }
    }
    builder.flush();
    while builder
        .lines
        .last()
        .is_some_and(|line| line.spans.is_empty())
    {
        builder.lines.pop();
    }
    let mut result = vec![];
    for line in builder.lines {
        result.extend(wrap(line, width));
    }
    if result.is_empty() {
        result.push(Line::default());
    }
    result
}

#[derive(Default)]
struct Builder {
    lines: Vec<Line<'static>>,
    pending: Vec<Span<'static>>,
    styles: Vec<Style>,
    quote: usize,
    lists: Vec<Option<u64>>,
    links: Vec<(String, bool)>,
    code: Option<(String, String)>,
    table: Option<Vec<Vec<Line<'static>>>>,
    cells: Vec<Line<'static>>,
}
impl Builder {
    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }
    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style().patch(style));
    }
    fn pop_style(&mut self) {
        self.styles.pop();
    }
    fn text(&mut self, text: &str, style: Style) {
        if !text.is_empty() {
            self.pending
                .push(Span::styled(text.replace('\t', "    "), style));
        }
    }
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut prefix = vec![];
        if self.quote > 0 {
            prefix.push(Span::styled(
                "│ ".repeat(self.quote),
                Style::default().fg(MUTED),
            ));
        }
        if self.lists.len() > 1 {
            prefix.push(Span::raw("  ".repeat(self.lists.len() - 1)));
        }
        prefix.append(&mut self.pending);
        self.lines.push(Line::from(prefix));
    }
    fn blank(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }
    fn table_lines(&mut self, table: Vec<Vec<Line<'static>>>, width: usize) {
        let columns = table.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let widths: Vec<usize> = (0..columns)
            .map(|col| {
                table
                    .iter()
                    .filter_map(|row| row.get(col))
                    .map(Line::width)
                    .max()
                    .unwrap_or(1)
                    .max(1)
                    .min(
                        width
                            .saturating_sub(columns * 3 + 1)
                            .checked_div(columns)
                            .unwrap_or(1)
                            .max(1),
                    )
            })
            .collect();
        for (index, row) in table.into_iter().enumerate() {
            let mut chunks: Vec<Vec<Line>> = (0..columns)
                .map(|col| wrap(row.get(col).cloned().unwrap_or_default(), widths[col]))
                .collect();
            let height = chunks.iter().map(Vec::len).max().unwrap_or(1);
            for offset in 0..height {
                self.text("│", Style::default().fg(MUTED));
                for (col, cell) in chunks.iter_mut().enumerate() {
                    self.text(" ", Style::default());
                    if let Some(line) = cell.get_mut(offset) {
                        let used = line.width();
                        self.pending.append(&mut line.spans);
                        self.text(
                            &" ".repeat(widths[col].saturating_sub(used)),
                            Style::default(),
                        );
                    } else {
                        self.text(&" ".repeat(widths[col]), Style::default());
                    }
                    self.text(" │", Style::default().fg(MUTED));
                }
                self.flush();
            }
            if index == 0 {
                self.text(
                    &format!(
                        "├{}┤",
                        widths
                            .iter()
                            .map(|w| "─".repeat(w + 2))
                            .collect::<Vec<_>>()
                            .join("┼")
                    ),
                    Style::default().fg(MUTED),
                );
                self.flush();
            }
        }
    }
}

/// Wrap styled text at whitespace or grapheme boundaries, preserving formatting.
fn wrap(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut output = vec![];
    let mut current: Vec<Span<'static>> = vec![];
    let mut used = 0;
    for span in line.spans {
        for grapheme in span.content.graphemes(true) {
            let size = grapheme.width();
            if used + size > width && !current.is_empty() {
                let mut seen_text = false;
                let mut split = None;
                for (i, part) in current.iter().enumerate() {
                    if part.content.trim().is_empty() && seen_text {
                        split = Some(i);
                    }
                    if !part.content.trim().is_empty() {
                        seen_text = true;
                    }
                }
                if let Some(split) = split {
                    let rest = current.split_off(split + 1);
                    current.pop();
                    output.push(Line::from(std::mem::replace(&mut current, rest)));
                    used = current.iter().map(Span::width).sum();
                } else {
                    output.push(Line::from(std::mem::take(&mut current)));
                    used = 0;
                }
            }
            if used + size > width && !current.is_empty() {
                output.push(Line::from(std::mem::take(&mut current)));
                used = 0;
            }
            current.push(Span::styled(grapheme.to_owned(), span.style));
            used += size;
        }
    }
    if !current.is_empty() || output.is_empty() {
        output.push(Line::from(current));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commonmark_styles_code_tables_and_unicode_wrap() {
        let source = "# Привет 🦀\n\n**bold** and *italic* with [link](https://example.com).\n\n> quote\n\n- [x] done\n- [ ] pending\n\n```rust\nfn main() {}\n```\n\n| Name | Value |\n| --- | --- |\n| Rust | 世界 |\n";
        let syntaxes = crate::filetype::syntax_set();
        let themes = syntect::highlighting::ThemeSet::load_defaults();
        let lines = render(source, 40, &syntaxes, &themes.themes["base16-ocean.dark"]);
        let text = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Привет 🦀",
            "bold",
            "italic",
            "quote",
            "☑",
            "☐",
            "fn main() {}",
            "Rust",
            "世界",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert!(!text.contains("**"));
        assert!(!text.contains("```"));
        assert!(
            lines
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(lines.iter().all(|l| l.width() <= 40));
        let narrow = render(
            "Привет 👩‍💻 世界 repeatedword",
            8,
            &syntaxes,
            &themes.themes["base16-ocean.dark"],
        );
        assert!(narrow.iter().all(|l| l.width() <= 8));
        assert!(
            narrow
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.content == "👩‍💻")
        );
    }
}
