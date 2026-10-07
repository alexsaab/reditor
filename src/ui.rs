use crate::{
    app::{App, Confirmation, Dialog},
    filetype,
    i18n::Language,
    markdown,
    mouse::{HitAreas, MouseController},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_image::{Image, Resize, picker::Picker, protocol::Protocol};
use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};
use unicode_width::UnicodeWidthStr;
#[path = "workspace_ui.rs"]
mod workspace_ui;

const BG: Color = Color::Rgb(17, 23, 34);
const PANEL: Color = Color::Rgb(23, 31, 45);
const TEXT: Color = Color::Rgb(212, 222, 239);
const MUTED: Color = Color::Rgb(113, 132, 160);
const ACCENT: Color = Color::Rgb(76, 207, 176);
const ERROR: Color = Color::Rgb(255, 124, 137);

#[derive(PartialEq, Eq)]
struct DiagramImageKey {
    id: u64,
    revision: u64,
    zoom: u16,
    pan_x: u32,
    pan_y: u32,
    width: u16,
    height: u16,
}

pub struct Renderer {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
    cache_key: Option<(u64, u64, Option<std::path::PathBuf>, u64)>,
    lines: Vec<Line<'static>>,
    pub mouse: MouseController,
    tab_start: usize,
    preview_key: Option<(u64, u64, u16)>,
    preview_lines: Vec<Line<'static>>,
    picker: Picker,
    diagram_key: Option<DiagramImageKey>,
    diagram_image: Option<Protocol>,
    studio_areas: workspace_ui::Areas,
    resize_drag: Option<workspace_ui::ResizeDrag>,
    resize_viewport: Rect,
    theme: String,
}
impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}
impl Renderer {
    pub fn new() -> Self {
        Self {
            syntaxes: filetype::syntax_set(),
            themes: ThemeSet::load_defaults(),
            cache_key: None,
            lines: vec![],
            mouse: MouseController::default(),
            tab_start: 0,
            preview_key: None,
            preview_lines: vec![],
            picker: Picker::halfblocks(),
            diagram_key: None,
            diagram_image: None,
            studio_areas: workspace_ui::Areas::default(),
            resize_drag: None,
            resize_viewport: Rect::default(),
            theme: String::new(),
        }
    }
    pub fn draw(&mut self, frame: &mut Frame, app: &mut App) {
        self.mouse.areas = HitAreas::default();
        self.studio_areas = workspace_ui::Areas::default();
        if self.theme != app.studio.config.theme {
            self.theme = app.studio.config.theme.clone();
            self.cache_key = None;
            self.preview_key = None;
        }
        let area = frame.area();
        if area != self.resize_viewport || app.dialog.is_some() {
            self.resize_drag = None;
        }
        self.resize_viewport = area;
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(TEXT)),
            area,
        );
        if area.width < 35 || area.height < 8 {
            frame.render_widget(
                Paragraph::new("reditor 🦀\n↔ 35 × ↕ 8\nCtrl+Q").style(Style::default().fg(ACCENT)),
                area,
            );
            return;
        }
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        let mut menu_x = rows[0].x;
        for (index, name) in crate::app::MENU_NAMES.iter().enumerate() {
            let label = format!(" {name} ");
            let width = label.width() as u16;
            let hit = Rect::new(menu_x, rows[0].y, width, 1);
            let active_menu =
                matches!(app.dialog, Some(Dialog::Menu { menu, .. }) if menu == index);
            frame.render_widget(
                Paragraph::new(label).style(
                    Style::default()
                        .fg(if active_menu { ACCENT } else { TEXT })
                        .bg(PANEL),
                ),
                hit,
            );
            self.mouse.areas.menu_headers.push((hit, index));
            menu_x += width;
        }
        let titles: Vec<Line> = app
            .documents
            .iter()
            .enumerate()
            .map(|(index, doc)| {
                let name = if doc.path.is_none() {
                    app.i18n.t("untitled").to_owned()
                } else {
                    doc.name()
                };
                Line::from(format!(
                    "{} {}{}{}",
                    index + 1,
                    name,
                    if doc.dirty() { " ●" } else { "" },
                    doc.path
                        .as_ref()
                        .and_then(|path| app.remote_binding(path))
                        .map_or("", |binding| {
                            if doc.disk_baseline() != Some(binding.baseline.as_str()) {
                                " ⇧"
                            } else {
                                " ☁"
                            }
                        })
                ))
            })
            .collect();
        self.tab_start = self.tab_start.min(app.active);
        while self.tab_start < app.active
            && titles[self.tab_start..=app.active]
                .iter()
                .map(|t| t.width() + 3)
                .sum::<usize>()
                > rows[1].width as usize
        {
            self.tab_start += 1;
        }
        frame.render_widget(Block::default().style(Style::default().bg(PANEL)), rows[1]);
        let mut x = rows[1].x;
        for (index, title) in titles.iter().enumerate().skip(self.tab_start) {
            if x >= rows[1].right() {
                break;
            }
            let width = (title.width() + 2).min((rows[1].right() - x) as usize) as u16;
            let hit = Rect::new(x, rows[1].y, width, 1);
            let style = if index == app.active {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED)
            };
            let mut padded = title.clone();
            padded.spans.insert(0, Span::raw(" "));
            padded.spans.push(Span::raw(" "));
            frame.render_widget(Paragraph::new(padded).style(style), hit);
            self.mouse.areas.tabs.push((hit, index));
            x += width;
            if x < rows[1].right() {
                frame.render_widget(
                    Paragraph::new("│").style(Style::default().fg(MUTED)),
                    Rect::new(x, rows[1].y, 1, 1),
                );
                x += 1;
            }
        }
        let workspace = if app.studio.bottom_visible && rows[2].height >= 10 {
            let parts = Layout::vertical([
                Constraint::Min(5),
                Constraint::Length(
                    app.studio
                        .bottom_height
                        .min(rows[1].height.saturating_sub(5)),
                ),
            ])
            .split(rows[2]);
            self.resize_border(
                Rect::new(parts[1].x, parts[1].y, parts[1].width, 1),
                workspace_ui::ResizeKind::Terminal,
                rows[2],
                parts[1].height,
            );
            self.bottom(frame, app, parts[1]);
            parts[0]
        } else {
            rows[2]
        };
        let rail =
            Layout::horizontal([Constraint::Length(6), Constraint::Min(10)]).split(workspace);
        self.activity_bar(frame, app, rail[0]);
        let columns = Layout::horizontal([
            Constraint::Length(
                app.studio
                    .explorer_width
                    .unwrap_or(if area.width >= 75 { 27 } else { 17 })
                    .clamp(12, rail[1].width.saturating_sub(10).max(12)),
            ),
            Constraint::Min(10),
        ])
        .split(rail[1]);
        self.explorer(frame, app, columns[0]);
        self.editors(frame, app, columns[1]);
        self.resize_border(
            Rect::new(
                columns[1].x.saturating_sub(1),
                workspace.y,
                2,
                workspace.height,
            ),
            workspace_ui::ResizeKind::Explorer,
            rail[1],
            columns[0].width,
        );
        self.resize_handles(frame);
        let (row, col) = app.doc().position();
        let text_language = app
            .studio
            .proofread
            .as_ref()
            .filter(|proofread| {
                proofread.id == app.doc().id && proofread.revision == app.doc().revision
            })
            .and_then(|proofread| proofread.language.as_deref())
            .map(|code| format!(" · {}", code.to_uppercase()))
            .unwrap_or_default();
        let position = format!(
            " {} {}:{}  {}  {}{} ",
            app.i18n.t("position"),
            row + 1,
            col + 1,
            filetype::detect(
                &self.syntaxes,
                app.doc().path.as_deref(),
                &app.doc().line(0)
            )
            .name,
            app.i18n.code(),
            text_language
        );
        let status_columns = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(position.width() as u16),
        ])
        .split(rows[3]);
        let status = if app.build.is_some() {
            app.i18n.t("running")
        } else {
            &app.status
        };
        let status = if app.studio.branch.is_empty() {
            status.to_owned()
        } else {
            format!(" {} · {status}", app.studio.branch)
        };
        frame.render_widget(
            Paragraph::new(format!(" {status}")).style(
                Style::default()
                    .bg(PANEL)
                    .fg(if app.status_error { ERROR } else { TEXT }),
            ),
            status_columns[0],
        );
        frame.render_widget(
            Paragraph::new(position).style(Style::default().bg(ACCENT).fg(BG)),
            status_columns[1],
        );
        let brand = " reditor 🦀 │ ";
        frame.render_widget(
            Paragraph::new(brand).style(Style::default().fg(MUTED)),
            rows[4],
        );
        let mut x = rows[4].x + (brand.width() as u16).min(rows[4].width);
        let keys = [
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
        ];
        for (label, key) in app.i18n.t("footer").split("  ").zip(keys) {
            if x >= rows[4].right() {
                break;
            }
            let label = format!(" {label} ");
            let width = (label.width() as u16).min(rows[4].right() - x);
            let hit = Rect::new(x, rows[4].y, width, 1);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(ACCENT).bg(PANEL)),
                hit,
            );
            self.mouse.areas.toolbar.push((hit, key));
            x += width + 1;
        }
        if app.dialog.is_some() {
            self.dialog(frame, app);
        }
        self.apply_theme(frame, app);
    }
    pub fn follow_cursor(&mut self) {
        self.resize_drag = None;
        self.mouse.keyboard();
    }
    pub fn query_graphics(&mut self) {
        if std::env::var("REDITOR_IMAGE_PROTOCOL").as_deref() != Ok("halfblocks") {
            self.picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        }
    }
    pub fn mouse(&mut self, app: &mut App, event: MouseEvent) {
        if self.studio_mouse(app, event) {
            return;
        }
        self.mouse.handle(app, event);
    }
    fn explorer(&mut self, frame: &mut Frame, app: &mut App, area: Rect) {
        let title = format!(
            " {} {} ",
            if app.explorer_focus { "▸" } else { " " },
            app.i18n.t("files")
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(if app.explorer_focus { ACCENT } else { MUTED }));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(inner);
        self.mouse.areas.explorer = area;
        self.mouse.areas.files = rows[1];
        self.mouse.areas.parent =
            Rect::new(rows[0].x, rows[0].y, rows[0].width, rows[0].height.min(1));
        frame.render_widget(
            Paragraph::new(format!("↑ ..\n{}", app.directory.to_string_lossy()))
                .style(Style::default().fg(MUTED)),
            rows[0],
        );
        let items: Vec<ListItem> = app
            .entries
            .iter()
            .map(|entry| {
                let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
                ListItem::new(format!(
                    "{} {name}{}",
                    if entry.directory { "▸" } else { "·" },
                    if entry.directory { "/" } else { "" }
                ))
                .style(Style::default().fg(if entry.directory {
                    ACCENT
                } else {
                    TEXT
                }))
            })
            .collect();
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(app.i18n.t("empty")).style(Style::default().fg(MUTED)),
                rows[1],
            );
            return;
        }
        let mut state = ListState::default()
            .with_offset(app.explorer_offset)
            .with_selected(Some(app.selected));
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::default().bg(PANEL).fg(ACCENT))
                .highlight_symbol("› "),
            rows[1],
            &mut state,
        );
        app.explorer_offset = state.offset();
    }
    fn editor(&mut self, frame: &mut Frame, app: &mut App, area: Rect) {
        let title = app
            .doc()
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| app.i18n.t("untitled").to_owned());
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .border_style(Style::default().fg(if app.explorer_focus { MUTED } else { ACCENT }));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if app.doc().can_preview() || app.doc().preview {
            let label = format!(
                " F7 {} ",
                app.i18n.t(if app.doc().preview {
                    "source_mode"
                } else {
                    "preview_mode"
                })
            );
            let width = (label.width() as u16).min(area.width.saturating_sub(2));
            self.mouse.areas.mode_toggle =
                Rect::new(area.right().saturating_sub(width + 1), area.y, width, 1);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(BG).bg(ACCENT)),
                self.mouse.areas.mode_toggle,
            );
        }
        if app.doc().preview {
            self.preview(frame, app, inner);
            return;
        }
        if inner.width < 4 || inner.height == 0 {
            return;
        }
        let gutter = ((app.doc().text.len_lines().max(1).ilog10() + 1) as u16 + 2)
            .min(inner.width.saturating_sub(1));
        let columns =
            Layout::horizontal([Constraint::Length(gutter), Constraint::Min(1)]).split(inner);
        self.mouse.areas.editor = inner;
        self.mouse.areas.text = columns[1];
        let (row, _) = app.doc().position();
        let visual = app.doc().visual_column();
        let doc = app.doc_mut();
        let height = inner.height as usize;
        let width = columns[1].width as usize;
        if self.mouse.follow_cursor && row < doc.scroll {
            doc.scroll = row;
        }
        if self.mouse.follow_cursor && row >= doc.scroll + height {
            doc.scroll = row + 1 - height;
        }
        if self.mouse.follow_cursor && visual < doc.horizontal {
            doc.horizontal = visual;
        }
        if self.mouse.follow_cursor && visual >= doc.horizontal + width {
            doc.horizontal = visual + 1 - width;
        }
        doc.scroll = doc.scroll.min(doc.text.len_lines().saturating_sub(1));
        let end = (doc.scroll + height).min(doc.text.len_lines());
        let key = (
            app.doc().id,
            app.doc().revision,
            app.doc().path.clone(),
            app.studio.proofread_generation,
        );
        if self.cache_key.as_ref() != Some(&key) || self.lines.len() < end {
            let syntax = filetype::detect(
                &self.syntaxes,
                app.doc().path.as_deref(),
                &app.doc().line(0),
            );
            let mut highlighter = HighlightLines::new(
                syntax,
                self.themes
                    .themes
                    .get(&app.studio.config.theme)
                    .unwrap_or(&self.themes.themes["base16-ocean.dark"]),
            );
            self.lines.clear();
            for index in 0..end {
                let text = app.doc().text.line(index).to_string();
                let spans: Vec<Span<'static>> = highlighter
                    .highlight_line(&text, &self.syntaxes)
                    .map(|ranges| {
                        ranges
                            .into_iter()
                            .map(|(style, content)| {
                                Span::styled(
                                    content.trim_end_matches(['\r', '\n']).replace('\t', "    "),
                                    Style::default().fg(Color::Rgb(
                                        style.foreground.r,
                                        style.foreground.g,
                                        style.foreground.b,
                                    )),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_else(|_| {
                        vec![Span::raw(
                            text.trim_end_matches(['\r', '\n']).replace('\t', "    "),
                        )]
                    });
                let spans = app
                    .studio
                    .proofread
                    .as_ref()
                    .filter(|proofread| {
                        proofread.id == app.doc().id && proofread.revision == app.doc().revision
                    })
                    .map(|proofread| {
                        proofread_spans(spans.clone(), &text, index, &proofread.diagnostics)
                    })
                    .unwrap_or(spans);
                self.lines.push(Line::from(spans));
            }
            self.cache_key = Some(key);
        }
        let doc = app.doc();
        if app.studio.config.wrap {
            self.wrapped_editor(frame, app, columns[0], columns[1]);
            return;
        }
        let numbers: Vec<Line> = (doc.scroll..end)
            .map(|i| {
                Line::from(Span::styled(
                    format!(
                        "{:>width$} ",
                        i + 1,
                        width = gutter.saturating_sub(1) as usize
                    ),
                    Style::default().fg(if i == row { ACCENT } else { MUTED }),
                ))
            })
            .collect();
        frame.render_widget(Paragraph::new(numbers), columns[0]);
        let mut visible = self.lines[doc.scroll..end].to_vec();
        if let Some(selection) = doc.selection() {
            for (index, line) in visible.iter_mut().enumerate() {
                let row = doc.scroll + index;
                let start = doc.text.line_to_char(row);
                let original = doc.line(row);
                let mut selected = Vec::new();
                for (offset, ch) in original.chars().enumerate() {
                    let repeat = if ch == '\t' { 4 } else { 1 };
                    selected.extend(std::iter::repeat_n(
                        selection.contains(&(start + offset)),
                        repeat,
                    ));
                }
                let mut offset = 0;
                let mut spans = vec![];
                for span in &line.spans {
                    for ch in span.content.chars() {
                        let style = if selected.get(offset).copied().unwrap_or(false) {
                            span.style.bg(Color::Rgb(55, 83, 112))
                        } else {
                            span.style
                        };
                        spans.push(Span::styled(ch.to_string(), style));
                        offset += 1;
                    }
                }
                line.spans = spans;
            }
        }
        frame.render_widget(
            Paragraph::new(visible).scroll((0, doc.horizontal.min(u16::MAX as usize) as u16)),
            columns[1],
        );
        if !app.explorer_focus
            && !app.studio.terminal_focus
            && app.dialog.is_none()
            && row >= doc.scroll
            && row < end
            && visual >= doc.horizontal
            && visual < doc.horizontal + width
        {
            let x = columns[1]
                .x
                .saturating_add((visual - doc.horizontal) as u16);
            let y = columns[1].y.saturating_add((row - doc.scroll) as u16);
            if x < columns[1].right() && y < columns[1].bottom() {
                frame.set_cursor_position((x, y));
            }
        }
    }
    fn preview(&mut self, frame: &mut Frame, app: &mut App, area: Rect) {
        self.mouse.areas.editor = area;
        self.mouse.areas.text = area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let key = (app.doc().id, app.doc().revision, area.width);
        if self.preview_key != Some(key) {
            self.preview_lines = markdown::render(
                &app.doc().text.to_string(),
                area.width as usize,
                &self.syntaxes,
                self.themes
                    .themes
                    .get(&app.studio.config.theme)
                    .unwrap_or(&self.themes.themes["base16-ocean.dark"]),
            );
            self.preview_key = Some(key);
        }
        self.mouse.areas.preview_rows = self.preview_lines.len();
        let max = self
            .preview_lines
            .len()
            .saturating_sub(area.height as usize);
        app.doc_mut().preview_scroll = app.doc().preview_scroll.min(max);
        let start = app.doc().preview_scroll;
        let end = (start + area.height as usize).min(self.preview_lines.len());
        frame.render_widget(
            Paragraph::new(self.preview_lines[start..end].to_vec()),
            area,
        );
    }
    fn dialog(&mut self, frame: &mut Frame, app: &mut App) {
        if matches!(app.dialog, Some(Dialog::Workbench)) {
            self.workbench(frame, app);
            return;
        }
        if matches!(app.dialog, Some(Dialog::Diagram)) {
            self.diagram(frame, app);
            return;
        }
        let full = frame.area();
        let width = full.width.saturating_sub(4).min(
            if matches!(app.dialog, Some(Dialog::Language { .. })) {
                50
            } else {
                90
            },
        );
        let requested_height = match app.dialog.as_ref().unwrap() {
            Dialog::Prompt { .. } | Dialog::Confirm(_) => 6,
            Dialog::Language { .. } => 9,
            Dialog::Menu { menu, .. } => (crate::app::MENU_ITEMS[*menu].len() as u16 + 2).min(18),
            _ => 32,
        };
        let height = full.height.saturating_sub(4).min(requested_height);
        let area = Rect::new(
            (full.width - width) / 2,
            (full.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let title = match app.dialog.as_ref().unwrap() {
            Dialog::Diagram | Dialog::Workbench => unreachable!(),
            Dialog::Prompt { kind, .. } => app.i18n.t(kind.key()),
            Dialog::Confirm(_) => app.i18n.t("dirty"),
            Dialog::Help { .. } => app.i18n.t("help"),
            Dialog::Plugins { .. } => app.i18n.t("plugins"),
            Dialog::Output { .. } => app.i18n.t("output"),
            Dialog::Language { .. } => app.i18n.t("language"),
            Dialog::Menu { menu, .. } => crate::app::MENU_NAMES[*menu],
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .style(Style::default().bg(PANEL).fg(TEXT))
            .border_style(Style::default().fg(ACCENT));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.mouse.areas.dialog = area;
        self.mouse.areas.dialog_close = Rect::new(area.right().saturating_sub(2), area.y, 1, 1);
        frame.render_widget(
            Paragraph::new("×").style(Style::default().fg(ACCENT).bg(PANEL)),
            self.mouse.areas.dialog_close,
        );
        let buttons = matches!(
            app.dialog,
            Some(
                Dialog::Prompt { .. }
                    | Dialog::Confirm(_)
                    | Dialog::Plugins { .. }
                    | Dialog::Language { .. }
            )
        );
        let content = if buttons {
            Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            )
        } else {
            inner
        };
        self.mouse.areas.dialog_content = content;
        if buttons && inner.height > 0 {
            let y = inner.bottom() - 1;
            let ok = format!("[ {} ]", app.i18n.t("confirm"));
            let cancel = format!("[ {} ]", app.i18n.t("cancel"));
            let width = (ok.width() as u16).min(inner.width);
            self.mouse.areas.confirm = Rect::new(inner.x, y, width, 1);
            let x = inner.x + width + 1;
            self.mouse.areas.cancel = Rect::new(
                x.min(inner.right()),
                y,
                (cancel.width() as u16).min(inner.right().saturating_sub(x)),
                1,
            );
            frame.render_widget(
                Paragraph::new(ok).style(Style::default().fg(BG).bg(ACCENT)),
                self.mouse.areas.confirm,
            );
            frame.render_widget(
                Paragraph::new(cancel).style(Style::default().fg(MUTED)),
                self.mouse.areas.cancel,
            );
        }
        match app.dialog.as_ref().unwrap() {
            Dialog::Diagram | Dialog::Workbench => unreachable!(),
            Dialog::Prompt { input, .. } => {
                let parts = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Min(0),
                ])
                .split(content);
                let available = parts[0].width.saturating_sub(1) as usize;
                let mut shown = String::new();
                for ch in input.chars().rev() {
                    let candidate = format!("{ch}{shown}");
                    if candidate.width() > available {
                        break;
                    }
                    shown = candidate;
                }
                frame.render_widget(
                    Paragraph::new(shown.clone()).style(Style::default().fg(ACCENT)),
                    parts[0],
                );
                frame.render_widget(
                    Paragraph::new(app.i18n.t("enter_hint")).style(Style::default().fg(MUTED)),
                    parts[2],
                );
                frame.set_cursor_position((parts[0].x + shown.width() as u16, parts[0].y));
            }
            Dialog::Confirm(action) => {
                let key = match action {
                    Confirmation::Quit => "confirm_quit",
                    Confirmation::Close => "confirm_close",
                    Confirmation::Overwrite(_) => "confirm_overwrite",
                };
                frame.render_widget(
                    Paragraph::new(app.i18n.t(key)).wrap(Wrap { trim: false }),
                    content,
                );
            }
            Dialog::Help { scroll } => frame.render_widget(
                Paragraph::new(app.i18n.t("help_text")).scroll((*scroll, 0)),
                inner,
            ),
            Dialog::Output { scroll } => {
                let output = if app.build_output.is_empty() {
                    app.i18n.t("no_output")
                } else {
                    &app.build_output
                };
                frame.render_widget(Paragraph::new(output).scroll((*scroll, 0)), inner);
            }
            Dialog::Plugins { selected } => {
                let commands = app.plugins.commands_for(&app.i18n.code().to_lowercase());
                if commands.is_empty() {
                    frame.render_widget(
                        Paragraph::new(app.i18n.t("no_plugins")).wrap(Wrap { trim: false }),
                        inner,
                    );
                } else {
                    let items: Vec<ListItem> = commands
                        .iter()
                        .map(|(_, _, title)| ListItem::new(title.as_str()))
                        .collect();
                    let mut state = ListState::default().with_selected(Some(*selected));
                    frame.render_stateful_widget(
                        List::new(items)
                            .highlight_style(Style::default().bg(ACCENT).fg(BG))
                            .highlight_symbol("› "),
                        content,
                        &mut state,
                    );
                    self.mouse.areas.plugin_offset = state.offset();
                }
            }
            Dialog::Language { selected } => {
                let items: Vec<ListItem> = Language::ALL
                    .iter()
                    .map(|language| {
                        ListItem::new(format!(
                            "{} {}",
                            if *language == app.i18n.language {
                                "✓"
                            } else {
                                " "
                            },
                            language.label()
                        ))
                    })
                    .collect();
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(
                    List::new(items)
                        .highlight_style(Style::default().bg(ACCENT).fg(BG))
                        .highlight_symbol("› "),
                    content,
                    &mut state,
                );
                self.mouse.areas.plugin_offset = state.offset();
            }
            Dialog::Menu { menu, selected } => {
                let items: Vec<ListItem> = crate::app::MENU_ITEMS[*menu]
                    .iter()
                    .map(|(label, shortcut, _)| ListItem::new(format!("{label:<32} {shortcut}")))
                    .collect();
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(
                    List::new(items)
                        .highlight_style(Style::default().bg(ACCENT).fg(BG))
                        .highlight_symbol("› "),
                    content,
                    &mut state,
                );
            }
        }
    }
    fn diagram(&mut self, frame: &mut Frame, app: &mut App) {
        let full = frame.area();
        let area = Rect::new(
            full.x + 1,
            full.y + 1,
            full.width.saturating_sub(2),
            full.height.saturating_sub(2),
        );
        let Some(view) = app.diagram.as_mut() else {
            return;
        };
        frame.render_widget(Clear, area);
        let mode = if view.graphics {
            format!("{:?}", self.picker.protocol_type())
        } else {
            app.i18n.t("diagram_text").to_owned()
        };
        let title = format!(
            " Mermaid {}/{} · {}% · {} ",
            view.selected + 1,
            view.blocks.len(),
            view.zoom,
            mode
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().bg(BG).fg(TEXT))
            .border_style(Style::default().fg(ACCENT));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.mouse.areas.dialog = area;
        self.mouse.areas.dialog_close = Rect::new(area.right().saturating_sub(2), area.y, 1, 1);
        frame.render_widget(
            Paragraph::new("×").style(Style::default().fg(ACCENT)),
            self.mouse.areas.dialog_close,
        );
        let rows = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
        self.mouse.areas.dialog_content = rows[0];
        let actions = [
            ("−", KeyCode::Char('-')),
            ("+", KeyCode::Char('+')),
            ("100%", KeyCode::Char('0')),
            ("‹", KeyCode::Char('[')),
            ("›", KeyCode::Char(']')),
            (app.i18n.t("diagram_mode"), KeyCode::Char('g')),
            (app.i18n.t("diagram_edit"), KeyCode::Char('e')),
            (app.i18n.t("cancel"), KeyCode::Esc),
        ];
        let mut x = rows[1].x;
        for (label, code) in actions {
            if x >= rows[1].right() {
                break;
            }
            let label = format!("[ {label} ]");
            let width = (label.width() as u16).min(rows[1].right() - x);
            let hit = Rect::new(x, rows[1].y, width, 1);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(ACCENT).bg(PANEL)),
                hit,
            );
            self.mouse.areas.diagram_actions.push((hit, code));
            x = x.saturating_add(width).saturating_add(1);
        }
        frame.render_widget(
            Paragraph::new(app.i18n.t("diagram_controls")).style(Style::default().fg(MUTED)),
            rows[2],
        );
        if let Some(error) = &view.error {
            frame.render_widget(
                Paragraph::new(format!(
                    "{}: {error}\n\n{}",
                    app.i18n.t("error"),
                    app.i18n.t("diagram_error_hint")
                ))
                .style(Style::default().fg(ERROR))
                .wrap(Wrap { trim: false }),
                rows[0],
            );
            return;
        }
        if !view.graphics
            && let Some(geometry) = &view.geometry
        {
            crate::diagram_text::render(
                frame,
                geometry,
                rows[0],
                view.zoom,
                (&mut view.pan_x, &mut view.pan_y),
            );
            return;
        }
        let Some(image) = &view.image else {
            frame.render_widget(
                Paragraph::new(app.i18n.t("diagram_rendering")).style(Style::default().fg(ACCENT)),
                rows[0],
            );
            return;
        };
        let target = rows[0];
        if target.width == 0 || target.height == 0 {
            return;
        }
        let font = self.picker.font_size();
        let pixel_width = u32::from(target.width) * u32::from(font.width.max(1));
        let pixel_height = u32::from(target.height) * u32::from(font.height.max(1));
        let fit = (pixel_width as f64 / image.width() as f64)
            .min(pixel_height as f64 / image.height() as f64);
        let scale = (fit * f64::from(view.zoom) / 100.0).max(0.001);
        let virtual_width = (f64::from(image.width()) * scale).ceil() as u32;
        let virtual_height = (f64::from(image.height()) * scale).ceil() as u32;
        view.pan_x = view.pan_x.min(virtual_width.saturating_sub(pixel_width));
        view.pan_y = view.pan_y.min(virtual_height.saturating_sub(pixel_height));
        let key = DiagramImageKey {
            id: view.id,
            revision: view.revision,
            zoom: view.zoom,
            pan_x: view.pan_x,
            pan_y: view.pan_y,
            width: target.width,
            height: target.height,
        };
        if self.diagram_key.as_ref() != Some(&key) {
            let crop_x = (f64::from(view.pan_x) / scale) as u32;
            let crop_y = (f64::from(view.pan_y) / scale) as u32;
            let crop_width = ((f64::from(pixel_width) / scale).ceil() as u32)
                .min(image.width().saturating_sub(crop_x))
                .max(1);
            let crop_height = ((f64::from(pixel_height) / scale).ceil() as u32)
                .min(image.height().saturating_sub(crop_y))
                .max(1);
            let cropped = image.crop_imm(crop_x, crop_y, crop_width, crop_height);
            let width = ((f64::from(crop_width) * scale).round() as u32)
                .min(pixel_width)
                .max(1);
            let height = ((f64::from(crop_height) * scale).round() as u32)
                .min(pixel_height)
                .max(1);
            let resized =
                cropped.resize_exact(width, height, image::imageops::FilterType::Triangle);
            match self.picker.new_protocol(
                resized,
                Size::new(target.width, target.height),
                Resize::Fit(None),
            ) {
                Ok(protocol) => {
                    self.diagram_image = Some(protocol);
                    self.diagram_key = Some(key);
                }
                Err(error) => {
                    frame.render_widget(Paragraph::new(error.to_string()), target);
                    return;
                }
            }
        }
        if let Some(image) = &self.diagram_image {
            frame.render_widget(Image::new(image), target);
        }
    }
}

fn proofread_spans(
    spans: Vec<Span<'static>>,
    line: &str,
    row: usize,
    diagnostics: &[crate::proofreader::Diagnostic],
) -> Vec<Span<'static>> {
    let line = line.trim_end_matches(['\r', '\n']);
    let column = |column: usize| -> usize {
        line.chars()
            .take(column)
            .map(|ch| if ch == '\t' { 4 } else { 1 })
            .sum()
    };
    let ranges: Vec<_> = diagnostics
        .iter()
        .filter_map(|diagnostic| {
            if row < diagnostic.line || row > diagnostic.end_line {
                return None;
            }
            let start = if row == diagnostic.line {
                column(diagnostic.column)
            } else {
                0
            };
            let end = if row == diagnostic.end_line {
                column(diagnostic.end_column)
            } else {
                column(line.chars().count())
            };
            (end > start).then_some((start, end, &diagnostic.kind))
        })
        .collect();
    if ranges.is_empty() {
        return spans;
    }
    let mut output = Vec::new();
    let mut offset = 0;
    for span in spans {
        let mut chunk = String::new();
        let mut active = None;
        for ch in span.content.chars() {
            let mut style = span.style;
            if let Some((_, _, kind)) = ranges
                .iter()
                .find(|(start, end, _)| offset >= *start && offset < *end)
            {
                let color = match kind {
                    crate::proofreader::IssueKind::Spelling => ERROR,
                    crate::proofreader::IssueKind::Grammar => Color::Rgb(111, 172, 255),
                };
                style = style
                    .fg(color)
                    .underline_color(color)
                    .add_modifier(Modifier::UNDERLINED);
            }
            if active != Some(style) && !chunk.is_empty() {
                output.push(Span::styled(std::mem::take(&mut chunk), active.unwrap()));
            }
            active = Some(style);
            chunk.push(ch);
            offset += 1;
        }
        if let Some(style) = active {
            output.push(Span::styled(chunk, style));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    #[test]
    fn proofreader_marks_spelling_and_grammar_in_editor_spans() {
        use crate::proofreader::{Diagnostic, IssueKind};
        let diagnostics = vec![
            Diagnostic {
                line: 0,
                column: 5,
                end_line: 0,
                end_column: 13,
                kind: IssueKind::Spelling,
                message: String::new(),
                replacements: vec![],
            },
            Diagnostic {
                line: 0,
                column: 18,
                end_line: 0,
                end_column: 23,
                kind: IssueKind::Grammar,
                message: String::new(),
                replacements: vec![],
            },
        ];
        let line = "This mistakke and wrong.";
        let spans = proofread_spans(vec![Span::raw(line)], line, 0, &diagnostics);
        assert!(spans.iter().any(|span| span.content == "mistakke"
            && span.style.add_modifier.contains(Modifier::UNDERLINED)
            && span.style.fg == Some(ERROR)));
        assert!(spans.iter().any(|span| span.content == "wrong"
            && span.style.add_modifier.contains(Modifier::UNDERLINED)
            && span.style.fg == Some(Color::Rgb(111, 172, 255))));
        assert!(spans.iter().any(|span| span.content.starts_with("This")
            && !span.style.add_modifier.contains(Modifier::UNDERLINED)));
    }
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    struct Fixture {
        root: tempfile::TempDir,
        app: App,
        renderer: Renderer,
        terminal: Terminal<TestBackend>,
    }
    impl Fixture {
        fn new() -> anyhow::Result<Self> {
            let root = tempfile::tempdir()?;
            let app = App::new(root.path().to_owned(), Language::En, vec![])?;
            Ok(Self {
                root,
                app,
                renderer: Renderer::new(),
                terminal: Terminal::new(TestBackend::new(100, 24))?,
            })
        }
        fn draw(&mut self) -> anyhow::Result<()> {
            self.terminal
                .draw(|frame| self.renderer.draw(frame, &mut self.app))?;
            Ok(())
        }
        fn mouse(&mut self, kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) {
            self.renderer.mouse(
                &mut self.app,
                MouseEvent {
                    kind,
                    column: x,
                    row: y,
                    modifiers,
                },
            );
        }
        fn click(&mut self, x: u16, y: u16) {
            self.mouse(
                MouseEventKind::Down(MouseButton::Left),
                x,
                y,
                KeyModifiers::NONE,
            );
            self.mouse(
                MouseEventKind::Up(MouseButton::Left),
                x,
                y,
                KeyModifiers::NONE,
            );
        }
    }
    #[test]
    fn mouse_cursor_selection_tabs_and_scrolling() -> anyhow::Result<()> {
        let mut f = Fixture::new()?;
        f.app.paste("a\t🦀世界z\n");
        f.app.doc_mut().cursor = 0;
        f.draw()?;
        let text = f.renderer.mouse.areas.text;
        f.click(text.x + 7, text.y);
        assert_eq!(f.app.doc().position(), (0, 3));
        f.mouse(
            MouseEventKind::Down(MouseButton::Left),
            text.x + 1,
            text.y,
            KeyModifiers::NONE,
        );
        f.mouse(
            MouseEventKind::Drag(MouseButton::Left),
            text.x + 9,
            text.y,
            KeyModifiers::NONE,
        );
        f.mouse(
            MouseEventKind::Up(MouseButton::Left),
            text.x + 9,
            text.y,
            KeyModifiers::NONE,
        );
        assert_eq!(f.app.doc().selected_text().as_deref(), Some("\t🦀世"));
        f.app
            .key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        f.draw()?;
        let tab = f.renderer.mouse.areas.tabs[0].0;
        f.click(tab.x, tab.y);
        assert_eq!(f.app.active, 0);
        f.app.doc_mut().replace(
            &(0..80)
                .map(|i| format!("{i:02} {}\n", "abcdef".repeat(30)))
                .collect::<String>(),
        );
        f.app.doc_mut().cursor = 0;
        f.renderer.follow_cursor();
        f.draw()?;
        let text = f.renderer.mouse.areas.text;
        f.mouse(
            MouseEventKind::ScrollDown,
            text.x,
            text.y,
            KeyModifiers::NONE,
        );
        f.draw()?;
        assert_eq!(f.app.doc().scroll, 3);
        assert_eq!(f.app.doc().cursor, 0);
        f.mouse(
            MouseEventKind::ScrollDown,
            text.x,
            text.y,
            KeyModifiers::SHIFT,
        );
        f.draw()?;
        assert_eq!(f.app.doc().horizontal, 3);
        f.click(text.x + 2, text.y + 1);
        assert_eq!(f.app.doc().position(), (4, 5));
        f.app.doc_mut().cursor = 0;
        f.renderer.follow_cursor();
        f.draw()?;
        assert_eq!(f.app.doc().scroll, 0);
        Ok(())
    }
    #[test]
    fn mouse_explorer_dialogs_and_double_click_word() -> anyhow::Result<()> {
        let mut f = Fixture::new()?;
        std::fs::write(
            f.root.path().join("test.ts"),
            "const привет: string = 'hello';\n",
        )?;
        f.app.refresh()?;
        f.draw()?;
        let files = f.renderer.mouse.areas.files;
        f.click(files.x + 3, files.y);
        assert!(f.app.explorer_focus);
        assert!(f.app.doc().path.is_none());
        f.click(files.x + 3, files.y);
        assert!(
            f.app
                .doc()
                .path
                .as_ref()
                .is_some_and(|p| p.ends_with("test.ts"))
        );
        f.draw()?;
        let text = f.renderer.mouse.areas.text;
        f.click(text.x + 8, text.y);
        f.click(text.x + 8, text.y);
        assert_eq!(f.app.doc().selected_text().as_deref(), Some("привет"));
        f.app
            .key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        f.app.paste("nested/directory");
        f.draw()?;
        let ok = f.renderer.mouse.areas.confirm;
        f.click(ok.x, ok.y);
        assert!(f.root.path().join("nested/directory").is_dir());
        assert!(f.app.dialog.is_none());
        f.app.dialog = Some(Dialog::Help { scroll: 0 });
        f.draw()?;
        let dialog = f.renderer.mouse.areas.dialog;
        f.mouse(
            MouseEventKind::ScrollDown,
            dialog.x + 1,
            dialog.y + 1,
            KeyModifiers::NONE,
        );
        assert!(matches!(f.app.dialog, Some(Dialog::Help { scroll: 3 })));
        f.draw()?;
        let close = f.renderer.mouse.areas.dialog_close;
        f.click(close.x, close.y);
        assert!(f.app.dialog.is_none());
        Ok(())
    }
    #[test]
    fn language_menu_supports_cancel_keyboard_and_mouse() -> anyhow::Result<()> {
        let mut f = Fixture::new()?;
        f.app.paste("unsaved text");
        f.app.key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert!(matches!(
            f.app.dialog,
            Some(Dialog::Language { selected: 1 })
        ));
        f.app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(f.app.i18n.language, Language::En);
        f.app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(f.app.i18n.language, Language::En);
        assert!(!f.root.path().join(".reditor/config.toml").exists());

        f.app.key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        f.draw()?;
        let buffer = f.terminal.backend().buffer();
        let output: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        for language in Language::ALL {
            assert!(output.contains(language.label()));
        }
        f.app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        f.app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(f.app.i18n.language, Language::De);
        assert!(f.app.dialog.is_none());
        let config_path = f.root.path().join(".reditor/config.toml");
        let config: crate::app::Config = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
        assert_eq!(config.language, Language::De);
        f.draw()?;
        let toolbar = f
            .renderer
            .mouse
            .areas
            .toolbar
            .iter()
            .find(|(_, key)| key.code == KeyCode::F(2))
            .unwrap()
            .0;
        f.click(toolbar.x, toolbar.y);
        f.draw()?;
        assert!(matches!(
            f.app.dialog,
            Some(Dialog::Language { selected: 2 })
        ));
        let content = f.renderer.mouse.areas.dialog_content;
        f.click(content.x + 2, content.y + 3);
        assert_eq!(f.app.i18n.language, Language::Es);
        assert!(f.app.dialog.is_none());
        let config: crate::app::Config = toml::from_str(&std::fs::read_to_string(config_path)?)?;
        assert_eq!(config.language, Language::Es);
        assert_eq!(f.app.doc().text.to_string(), "unsaved text");
        assert!(f.app.doc().dirty());
        Ok(())
    }
    #[test]
    fn markdown_preview_is_read_only_and_preserves_editing_state() -> anyhow::Result<()> {
        let mut f = Fixture::new()?;
        let path = f.root.path().join("preview.md");
        let source = "# Заголовок\n\n**Жирный** и *курсив*\n\n".to_owned()
            + &(0..80)
                .map(|i| format!("- Строка {i}\n"))
                .collect::<String>();
        std::fs::write(&path, &source)?;
        f.app.open(&path)?;
        f.app.doc_mut().cursor = 2;
        f.app.doc_mut().scroll = 0;
        f.app.key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
        f.draw()?;
        assert!(f.app.doc().preview);
        let output: String = f
            .terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(output.contains("Заголовок"));
        assert!(!output.contains("# Заголовок"));
        assert!(!output.contains("**Жирный**"));
        f.app.paste("must not be inserted");
        f.app
            .key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(f.app.doc().text.to_string(), source);
        assert_eq!(f.app.doc().cursor, 2);
        assert!(!f.app.doc().dirty());
        f.app
            .key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        f.draw()?;
        assert_eq!(f.app.doc().preview_scroll, 15);
        let text = f.renderer.mouse.areas.text;
        f.mouse(
            MouseEventKind::ScrollDown,
            text.x,
            text.y,
            KeyModifiers::NONE,
        );
        f.draw()?;
        assert_eq!(f.app.doc().preview_scroll, 18);
        f.app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        f.draw()?;
        assert_eq!(
            f.app.doc().preview_scroll,
            f.renderer.preview_lines.len() - text.height as usize
        );
        let toggle = f.renderer.mouse.areas.mode_toggle;
        f.click(toggle.x, toggle.y);
        f.draw()?;
        assert!(!f.app.doc().preview);
        assert_eq!(f.app.doc().cursor, 2);
        f.app.doc_mut().insert("changed");
        f.app.key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
        f.draw()?;
        assert!(
            f.renderer
                .preview_lines
                .iter()
                .flat_map(|l| &l.spans)
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .contains("changed")
        );
        Ok(())
    }
    #[test]
    fn mermaid_view_renders_inside_terminal_and_returns_to_source() -> anyhow::Result<()> {
        let mut f = Fixture::new()?;
        f.app.paste("```mermaid\nflowchart LR\n A --> B\n```\n");
        let blocks = crate::diagram::blocks(f.app.doc());
        let start = blocks[0].start;
        let mut view = crate::diagram::DiagramView::new(blocks, 0);
        view.geometry = Some(crate::diagram::Geometry {
            width: 1400.0,
            height: 800.0,
            labels: vec![crate::diagram::Label {
                text: "Привет Rust 🦀".into(),
                x: 50.0,
                y: 40.0,
                width: 160.0,
                height: 20.0,
            }],
            paths: vec![vec![[20.0, 20.0], [200.0, 20.0], [200.0, 90.0]]],
        });
        view.image = Some(image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(
            400,
            200,
            |x, y| {
                if x > 50 && x < 350 && y > 40 && y < 150 {
                    image::Rgb([76, 207, 176])
                } else {
                    image::Rgb([17, 23, 34])
                }
            },
        )));
        f.app.diagram = Some(view);
        f.app.dialog = Some(Dialog::Diagram);
        f.draw()?;
        let output: String = f
            .terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(output.contains("Mermaid 1/1"));
        assert!(output.contains("Привет Rust 🦀"));
        assert!(f.renderer.diagram_image.is_none());
        let content = f.renderer.mouse.areas.dialog_content;
        f.renderer.mouse(
            &mut f.app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: content.x + 1,
                row: content.y + 1,
                modifiers: KeyModifiers::SHIFT,
            },
        );
        f.draw()?;
        assert_eq!(f.app.diagram.as_ref().unwrap().pan_x, 40);
        f.renderer.mouse(
            &mut f.app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: content.x + 1,
                row: content.y + 1,
                modifiers: KeyModifiers::NONE,
            },
        );
        f.draw()?;
        assert_eq!(f.app.diagram.as_ref().unwrap().pan_y, 40);
        let original = f.app.doc().text.to_string();
        f.app
            .key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
        f.draw()?;
        assert_eq!(f.app.diagram.as_ref().unwrap().zoom, 125);
        f.app
            .key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        f.draw()?;
        assert!(f.renderer.diagram_image.is_some());
        assert!(f.app.diagram.as_ref().unwrap().graphics);
        f.app
            .key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        f.draw()?;
        assert!(!f.app.diagram.as_ref().unwrap().graphics);
        let edit = f
            .renderer
            .mouse
            .areas
            .diagram_actions
            .iter()
            .find(|(_, key)| *key == KeyCode::Char('e'))
            .unwrap()
            .0;
        f.click(edit.x, edit.y);
        assert!(f.app.dialog.is_none());
        assert!(f.app.diagram.is_none());
        assert!(!f.app.doc().preview);
        assert_eq!(f.app.doc().text.char_to_byte(f.app.doc().cursor), start);
        assert_eq!(f.app.doc().text.to_string(), original);
        Ok(())
    }
    #[test]
    fn requested_formats_render_with_colors_and_save() -> anyhow::Result<()> {
        let mut f = Fixture::new()?;
        for (name, source) in [
            ("README.md", "# Заголовок\n**bold** `code`\n"),
            ("app.js", "const x = 'Привет';\n"),
            ("app.ts", "const x: string = 'Привет';\n"),
            ("index.html", "<h1 class=\"hello\">Привет</h1>\n"),
            ("Cargo.toml", "[package]\nname = \"demo\"\n"),
            ("Cargo.lock", "version = 4\n[[package]]\nname = \"demo\"\n"),
            (
                "settings.ron",
                "Settings(title: \"Привет\", active: true)\n",
            ),
            (
                "build.rs",
                "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }\n",
            ),
        ] {
            let path = f.root.path().join(name);
            std::fs::write(&path, source)?;
            f.app.open(&path)?;
            f.renderer.follow_cursor();
            f.draw()?;
            let colors: std::collections::HashSet<Color> = f
                .renderer
                .lines
                .iter()
                .flat_map(|line| line.spans.iter().filter_map(|s| s.style.fg))
                .collect();
            assert!(colors.len() > 1, "No syntax colors for {name}");
            f.app.doc_mut().cursor = f.app.doc().text.len_chars();
            f.app.paste("\n");
            f.app.save(&path, false)?;
            assert_eq!(std::fs::read_to_string(&path)?, format!("{source}\n"));
        }
        Ok(())
    }
    #[test]
    fn renders_all_locales_and_small_terminals() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let mut renderer = Renderer::new();
        for language in [Language::Ru, Language::En, Language::De, Language::Es] {
            let mut app = App::new(root.path().to_owned(), language, vec![])?;
            app.paste("fn main() {\n    println!(\"Привет 🦀\");\n}\n");
            let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
            terminal.draw(|frame| renderer.draw(frame, &mut app))?;
            let buffer = terminal.backend().buffer();
            let output: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(output.contains(app.i18n.t("files")));
            assert!(output.contains("println!"));
            for dialog in [
                Dialog::Help { scroll: 0 },
                Dialog::Plugins { selected: 0 },
                Dialog::Output { scroll: 0 },
            ] {
                app.dialog = Some(dialog);
                terminal.draw(|frame| renderer.draw(frame, &mut app))?;
            }
            for (width, height) in [(20, 5), (35, 8), (60, 15)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height))?;
                terminal.draw(|frame| renderer.draw(frame, &mut app))?;
            }
        }
        Ok(())
    }
}
