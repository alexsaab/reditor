use super::*;
use crate::studio::PanelMode;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::Position;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub(super) struct Areas {
    terminal: Rect,
    activity: Rect,
    activity_plugins: Vec<(Rect, usize)>,
    activity_explorer: Rect,
    activity_settings: Rect,
    terminal_tabs: Vec<(Rect, usize)>,
    buttons: Vec<(Rect, String)>,
    choices: Vec<(Rect, usize)>,
    secondary: Option<(usize, HitAreas)>,
    split_preview: Rect,
    resize: Vec<ResizeRegion>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResizeKind {
    Explorer,
    Split,
    Terminal,
}
#[derive(Clone, Copy)]
struct ResizeRegion {
    hit: Rect,
    kind: ResizeKind,
    bounds: Rect,
    size: u16,
}
pub(super) struct ResizeDrag {
    region: ResizeRegion,
    origin: Position,
}

impl Renderer {
    pub(super) fn activity_bar(&mut self, frame: &mut Frame, app: &mut App, area: Rect) {
        self.studio_areas.activity = area;
        frame.render_widget(Block::default().style(Style::default().bg(PANEL)), area);
        if area.height < 3 || area.width < 6 {
            return;
        }
        let mark = Rect::new(area.x, area.y, area.width - 1, 1);
        self.studio_areas.activity_explorer = mark;
        frame.render_widget(
            Paragraph::new("EX").style(
                Style::default()
                    .fg(if app.explorer_focus { ACCENT } else { MUTED })
                    .bg(PANEL),
            ),
            Rect::new(area.x + 2, area.y, 2, 1),
        );
        let visible = usize::from(area.height.saturating_sub(2) / 2);
        let count = app.plugins.plugins.len();
        app.studio.activity_scroll = app
            .studio
            .activity_scroll
            .min(count.saturating_sub(visible));
        for (row, index) in (app.studio.activity_scroll..count)
            .take(visible)
            .enumerate()
        {
            let plugin = &app.plugins.plugins[index];
            let (fallback, color) = match plugin.manifest.name.as_str() {
                "php" => ("PH", Color::Rgb(180, 165, 235)),
                "git" => ("GT", Color::Rgb(239, 142, 108)),
                "remote" => ("FT", Color::Rgb(113, 187, 236)),
                "formatter" => ("FM", ACCENT),
                "proofreader" => ("PR", Color::Rgb(223, 187, 117)),
                _ => ("", TEXT),
            };
            let symbol = plugin
                .manifest
                .icon
                .as_deref()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if fallback.is_empty() {
                        plugin
                            .manifest
                            .name
                            .chars()
                            .take(2)
                            .collect::<String>()
                            .to_uppercase()
                    } else {
                        fallback.into()
                    }
                });
            let selected = app.studio.activity_plugin == Some(index);
            let y = area.y + row as u16 * 2 + 1;
            let hit = Rect::new(area.x, y, area.width - 1, 2);
            if selected {
                frame.render_widget(
                    Paragraph::new("┃").style(Style::default().fg(ACCENT).bg(PANEL)),
                    Rect::new(area.x, y, 1, 1),
                );
            }
            frame.render_widget(
                Paragraph::new(symbol).style(if selected {
                    Style::default()
                        .fg(BG)
                        .bg(color)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(color)
                        .bg(PANEL)
                        .add_modifier(Modifier::BOLD)
                }),
                Rect::new(area.x + 2, y, 2, 1),
            );
            self.studio_areas.activity_plugins.push((hit, index));
        }
        let settings = Rect::new(area.x, area.bottom() - 1, area.width - 1, 1);
        self.studio_areas.activity_settings = settings;
        frame.render_widget(
            Paragraph::new("ST").style(Style::default().fg(MUTED).bg(PANEL)),
            Rect::new(area.x + 2, area.bottom() - 1, 2, 1),
        );
        for y in area.y..area.bottom() {
            frame.render_widget(
                Paragraph::new("│").style(Style::default().fg(MUTED).bg(PANEL)),
                Rect::new(area.right() - 1, y, 1, 1),
            );
        }
    }
    pub(super) fn resize_border(&mut self, hit: Rect, kind: ResizeKind, bounds: Rect, size: u16) {
        self.studio_areas.resize.push(ResizeRegion {
            hit,
            kind,
            bounds,
            size,
        });
    }
    pub(super) fn resize_handles(&self, frame: &mut Frame) {
        for region in &self.studio_areas.resize {
            let (x, y, symbol) = if region.kind == ResizeKind::Terminal {
                (region.hit.x + region.hit.width / 2, region.hit.y, "↕")
            } else {
                (region.hit.x, region.hit.y + region.hit.height / 2, "↔")
            };
            frame.render_widget(
                Paragraph::new(symbol).style(Style::default().fg(ACCENT)),
                Rect::new(x, y, 1, 1),
            );
        }
    }
    fn resize_mouse(&mut self, app: &mut App, event: MouseEvent, point: Position) -> bool {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.resize_drag = None;
                if let Some(region) = self
                    .studio_areas
                    .resize
                    .iter()
                    .find(|r| r.hit.contains(point))
                    .copied()
                {
                    self.mouse.keyboard();
                    self.resize_drag = Some(ResizeDrag {
                        region,
                        origin: point,
                    });
                    return true;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(drag) = &self.resize_drag {
                    let region = drag.region;
                    let delta = if region.kind == ResizeKind::Terminal {
                        i32::from(drag.origin.y) - i32::from(point.y)
                    } else {
                        i32::from(point.x) - i32::from(drag.origin.x)
                    };
                    let (minimum, maximum) = match region.kind {
                        ResizeKind::Terminal => (5, region.bounds.height.saturating_sub(5)),
                        ResizeKind::Explorer => (12, region.bounds.width.saturating_sub(10)),
                        ResizeKind::Split => {
                            let min = 12.min(region.bounds.width / 2);
                            (min, region.bounds.width.saturating_sub(min))
                        }
                    };
                    let size = (i32::from(region.size) + delta)
                        .clamp(i32::from(minimum), i32::from(maximum.max(minimum)))
                        as u16;
                    match region.kind {
                        ResizeKind::Terminal => app.studio.bottom_height = size,
                        ResizeKind::Explorer => app.studio.explorer_width = Some(size),
                        ResizeKind::Split => {
                            app.studio.split_percent = ((u32::from(size) * 100
                                + u32::from(region.bounds.width) / 2)
                                / u32::from(region.bounds.width.max(1)))
                                as u16
                        }
                    }
                    return true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.resize_drag.take().is_some() => {
                return true;
            }
            _ => {}
        }
        false
    }
    pub(super) fn editors(&mut self, frame: &mut Frame, app: &mut App, area: Rect) {
        if let Some(index) = app.studio.split.filter(|i| *i < app.documents.len()) {
            let minimum = 12.min(area.width / 2);
            let left = ((u32::from(area.width) * u32::from(app.studio.split_percent) / 100) as u16)
                .clamp(minimum, area.width.saturating_sub(minimum));
            let columns =
                Layout::horizontal([Constraint::Length(left), Constraint::Min(0)]).split(area);
            self.resize_border(
                Rect::new(columns[1].x.saturating_sub(1), area.y, 2, area.height),
                ResizeKind::Split,
                area,
                columns[0].width,
            );
            let active = if app.studio.split_right_focus {
                columns[1]
            } else {
                columns[0]
            };
            let passive = if app.studio.split_right_focus {
                columns[0]
            } else {
                columns[1]
            };
            if app.studio.split_mermaid {
                let block = Block::bordered()
                    .title(" Mermaid ")
                    .border_style(Style::default().fg(ACCENT));
                let inner = block.inner(columns[1]);
                frame.render_widget(block, columns[1]);
                if let Some(view) = app.diagram.as_mut() {
                    if let Some(geometry) = &view.geometry {
                        crate::diagram_text::render(
                            frame,
                            geometry,
                            inner,
                            view.zoom,
                            (&mut view.pan_x, &mut view.pan_y),
                        );
                    } else {
                        frame.render_widget(
                            Paragraph::new(
                                view.error
                                    .clone()
                                    .unwrap_or_else(|| app.i18n.t("diagram_rendering").into()),
                            )
                            .wrap(Wrap { trim: false }),
                            inner,
                        );
                    }
                }
                self.studio_areas.split_preview = columns[1];
            } else {
                let original = app.active;
                let areas = self.mouse.areas.clone();
                app.active = index;
                let preview = app.doc().preview;
                if app.studio.split_preview {
                    app.doc_mut().preview = true;
                }
                let focus = app.explorer_focus;
                app.explorer_focus = true;
                self.editor(frame, app, passive);
                app.explorer_focus = focus;
                app.doc_mut().preview = preview;
                if app.studio.split_preview {
                    self.studio_areas.split_preview = columns[1];
                } else {
                    self.studio_areas.secondary = Some((index, self.mouse.areas.clone()));
                }
                app.active = original;
                self.mouse.areas = areas;
            }
            self.editor(frame, app, active);
        } else {
            self.editor(frame, app, area);
        }
        self.diagnostic_gutter(frame, app);
    }
    fn diagnostic_gutter(&self, frame: &mut Frame, app: &App) {
        let text = self.mouse.areas.text;
        if text.x == 0 || app.doc().preview {
            return;
        }
        let Some(path) = &app.doc().path else {
            return;
        };
        if let Some(client) = &app.studio.lsp
            && let Ok(uri) = crate::lsp::uri(path)
            && let Some(diagnostics) = client.diagnostics.get(&uri)
        {
            for diagnostic in diagnostics {
                let row = diagnostic["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
                if row >= app.doc().scroll && row < app.doc().scroll + usize::from(text.height) {
                    frame.render_widget(
                        Paragraph::new("!").style(Style::default().fg(
                            if diagnostic["severity"].as_u64() == Some(1) {
                                ERROR
                            } else {
                                Color::Yellow
                            },
                        )),
                        Rect::new(text.x - 1, text.y + (row - app.doc().scroll) as u16, 1, 1),
                    );
                }
            }
            if let Some(message) = diagnostics.iter().find(|d| {
                d["range"]["start"]["line"].as_u64() == Some(app.doc().position().0 as u64)
            }) {
                let area = self.mouse.areas.editor;
                frame.render_widget(
                    Paragraph::new(message["message"].as_str().unwrap_or("").replace('\n', " "))
                        .style(Style::default().fg(ERROR).bg(BG)),
                    Rect::new(area.x, area.bottom(), area.width, 1),
                );
            }
        }
        if let Some(changes) = app.studio.git_lines.get(path) {
            for (row, symbol) in changes {
                if *row >= app.doc().scroll && *row < app.doc().scroll + usize::from(text.height) {
                    frame.render_widget(
                        Paragraph::new(symbol.to_string()).style(Style::default().fg(ACCENT)),
                        Rect::new(
                            text.x.saturating_sub(2),
                            text.y + (*row - app.doc().scroll) as u16,
                            1,
                            1,
                        ),
                    );
                }
            }
        }
    }
    pub(super) fn bottom(&mut self, frame: &mut Frame, app: &mut App, area: Rect) {
        let block = Block::bordered()
            .title(format!(" {} · F10 ", app.i18n.t("terminal")))
            .border_style(Style::default().fg(if app.studio.terminal_focus {
                ACCENT
            } else {
                MUTED
            }));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
        let buttons = [
            ("terminal_new", "terminal.new"),
            ("cargo_build", "cargo.build"),
            ("cargo_run", "cargo.run"),
            ("cargo_test", "cargo.test"),
            ("cargo_clippy", "cargo.clippy"),
            ("stop", "terminal.stop"),
            ("terminal_close", "terminal.close"),
            ("palette", "palette"),
        ];
        let mut x = rows[0].x;
        for (key, command) in buttons {
            let label = format!("[{}]", app.i18n.t(key));
            let width = label
                .width()
                .min(usize::from(rows[0].right().saturating_sub(x))) as u16;
            if width == 0 {
                break;
            }
            let hit = Rect::new(x, rows[0].y, width, 1);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(ACCENT).bg(PANEL)),
                hit,
            );
            self.studio_areas.buttons.push((hit, command.into()));
            x += width + 1;
        }
        x = rows[1].x;
        for (index, terminal) in app.studio.terminals.iter().enumerate() {
            if x >= rows[1].right() {
                break;
            }
            let label = format!(
                " {}:{}{} ",
                index + 1,
                terminal.title,
                if terminal.exit_code == Some(0) {
                    " ✓"
                } else if terminal.exit.is_some() {
                    " ×"
                } else {
                    ""
                }
            );
            let width = label.width().min(usize::from(rows[1].right() - x)) as u16;
            let hit = Rect::new(x, rows[1].y, width, 1);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(
                    if index == app.studio.terminal_active && !app.studio.bottom_plugin {
                        ACCENT
                    } else {
                        MUTED
                    },
                )),
                hit,
            );
            self.studio_areas.terminal_tabs.push((hit, index));
            x += width + 1;
        }
        if let Some(panel) = &app.studio.plugin_panel {
            let width = panel
                .title
                .width()
                .min(usize::from(rows[1].right().saturating_sub(x))) as u16;
            let hit = Rect::new(x, rows[1].y, width, 1);
            frame.render_widget(
                Paragraph::new(panel.title.clone()).style(Style::default().fg(ACCENT)),
                hit,
            );
            self.studio_areas.buttons.push((hit, "plugin.panel".into()));
        }
        self.studio_areas.terminal = rows[2];
        if app.studio.bottom_plugin {
            if let Some(panel) = &app.studio.plugin_panel {
                frame.render_widget(
                    Paragraph::new(panel.content.clone()).wrap(Wrap { trim: false }),
                    rows[2],
                );
            }
            return;
        }
        let focus = app.studio.terminal_focus && app.dialog.is_none();
        if let Some(terminal) = app.studio.terminal_mut() {
            terminal.resize(rows[2].height, rows[2].width);
            let screen = terminal.parser.screen();
            for row in 0..rows[2].height {
                for col in 0..rows[2].width {
                    if let Some(cell) = screen.cell(row, col) {
                        if cell.is_wide_continuation() {
                            continue;
                        }
                        let mut style = Style::default()
                            .fg(terminal_color(cell.fgcolor(), TEXT))
                            .bg(terminal_color(cell.bgcolor(), BG));
                        if cell.bold() {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if cell.italic() {
                            style = style.add_modifier(Modifier::ITALIC);
                        }
                        if cell.underline() {
                            style = style.add_modifier(Modifier::UNDERLINED);
                        }
                        if cell.inverse() {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                        let contents = cell.contents();
                        frame.buffer_mut().set_stringn(
                            rows[2].x + col,
                            rows[2].y + row,
                            if contents.is_empty() { " " } else { contents },
                            usize::from(rows[2].width - col),
                            style,
                        );
                    }
                }
            }
            if focus && !screen.hide_cursor() && screen.scrollback() == 0 {
                let (row, col) = screen.cursor_position();
                if row < rows[2].height && col < rows[2].width {
                    frame.set_cursor_position((rows[2].x + col, rows[2].y + row));
                }
            }
            let status = terminal
                .exit
                .as_deref()
                .unwrap_or("Ctrl+C · F10 · Alt+1…9 · Shift+PgUp/PgDn");
            frame.render_widget(
                Paragraph::new(status).style(Style::default().fg(MUTED)),
                rows[3],
            );
        }
    }
    pub(super) fn workbench(&mut self, frame: &mut Frame, app: &mut App) {
        let Some(panel) = &app.studio.panel else {
            return;
        };
        let full = frame.area();
        let width = full.width.saturating_sub(4).min(110);
        let height = full.height.saturating_sub(4).min(32);
        let area = Rect::new(
            full.x + (full.width - width) / 2,
            full.y + (full.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let block = Block::bordered()
            .title(format!(" {} ", panel.title))
            .style(Style::default().bg(PANEL).fg(TEXT))
            .border_style(Style::default().fg(ACCENT));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.mouse.areas.dialog = area;
        self.mouse.areas.dialog_close = Rect::new(area.right().saturating_sub(2), area.y, 1, 1);
        frame.render_widget(Paragraph::new("×"), self.mouse.areas.dialog_close);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
        self.mouse.areas.dialog_content = rows[1];
        let shown = format!("> {}", panel.input);
        frame.render_widget(
            Paragraph::new(shown.clone()).style(Style::default().fg(ACCENT)),
            rows[0],
        );
        if matches!(panel.mode, PanelMode::Text) {
            frame.render_widget(
                Paragraph::new(panel.body.clone())
                    .wrap(Wrap { trim: false })
                    .scroll((panel.scroll.min(u16::MAX as usize) as u16, 0)),
                rows[1],
            );
        } else if matches!(panel.mode, PanelMode::Review) {
            let parts =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(rows[1]);
            if let Some(item) = panel.items.first() {
                frame.render_widget(
                    Paragraph::new(format!("[ {} ]", item.label))
                        .style(Style::default().fg(BG).bg(ACCENT)),
                    parts[0],
                );
                self.studio_areas.choices.push((parts[0], 0));
            }
            frame.render_widget(
                Paragraph::new(panel.body.as_str())
                    .wrap(Wrap { trim: false })
                    .scroll((panel.scroll.min(u16::MAX as usize) as u16, 0)),
                parts[1],
            );
        } else {
            let filtered = panel.filtered();
            let start = panel
                .selected
                .saturating_sub(usize::from(rows[1].height).saturating_sub(1));
            for (row, index) in filtered
                .iter()
                .enumerate()
                .skip(start)
                .take(usize::from(rows[1].height))
            {
                let hit = Rect::new(
                    rows[1].x,
                    rows[1].y + (row - start) as u16,
                    rows[1].width,
                    1,
                );
                let item = &panel.items[*index];
                frame.render_widget(
                    Paragraph::new(format!(
                        "{} {}",
                        if row == panel.selected { "›" } else { " " },
                        item.label
                    ))
                    .style(Style::default().fg(if row == panel.selected {
                        ACCENT
                    } else {
                        TEXT
                    })),
                    hit,
                );
                self.studio_areas.choices.push((hit, row));
            }
            if filtered.is_empty() && !matches!(panel.mode, PanelMode::Input(_)) {
                frame.render_widget(Paragraph::new(app.i18n.t("no_results")), rows[1]);
            }
        }
        frame.render_widget(
            Paragraph::new(app.i18n.t("workbench_hint")).style(Style::default().fg(MUTED)),
            rows[2],
        );
        if !matches!(panel.mode, PanelMode::Text | PanelMode::Review) {
            frame.set_cursor_position((
                rows[0].x
                    + shown
                        .width()
                        .min(usize::from(rows[0].width.saturating_sub(1)))
                        as u16,
                rows[0].y,
            ));
        }
    }
    pub(super) fn studio_mouse(&mut self, app: &mut App, event: MouseEvent) -> bool {
        let point = Position::new(event.column, event.row);
        if app.dialog.is_some() {
            self.resize_drag = None;
        } else if self.resize_mouse(app, event, point) {
            return true;
        }
        if matches!(app.dialog, Some(Dialog::Workbench)) {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if self.mouse.areas.dialog_close.contains(point) {
                        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                    } else if let Some((_, row)) = self
                        .studio_areas
                        .choices
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        if let Some(panel) = app.studio.panel.as_mut() {
                            panel.selected = *row;
                        }
                        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    }
                }
                MouseEventKind::ScrollDown => {
                    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
                }
                MouseEventKind::ScrollUp => app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
                _ => {}
            }
            return true;
        }
        if app.dialog.is_some() {
            return false;
        }
        if app.studio.config.wrap
            && !app.doc().preview
            && self.mouse.areas.text.contains(point)
            && matches!(
                event.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
        {
            self.mouse.follow_cursor = false;
            app.doc_mut().wrap_scroll = if event.kind == MouseEventKind::ScrollUp {
                app.doc().wrap_scroll.saturating_sub(3)
            } else {
                app.doc().wrap_scroll.saturating_add(3)
            };
            return true;
        }
        if self.studio_areas.activity.contains(point) {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if self.studio_areas.activity_explorer.contains(point) {
                        app.explorer_focus = true;
                        app.studio.terminal_focus = false;
                    } else if self.studio_areas.activity_settings.contains(point) {
                        if let Err(error) = app.command("settings") {
                            app.error(error);
                        }
                    } else if let Some((_, index)) = self
                        .studio_areas
                        .activity_plugins
                        .iter()
                        .find(|(area, _)| area.contains(point))
                        && let Err(error) = app.show_plugin_commands(*index)
                    {
                        app.error(error);
                    }
                }
                MouseEventKind::ScrollUp => {
                    app.studio.activity_scroll = app.studio.activity_scroll.saturating_sub(2)
                }
                MouseEventKind::ScrollDown => {
                    app.studio.activity_scroll = app
                        .studio
                        .activity_scroll
                        .saturating_add(2)
                        .min(app.plugins.plugins.len().saturating_sub(1))
                }
                _ => {}
            }
            return true;
        }
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some((_, command)) = self
                .studio_areas
                .buttons
                .iter()
                .find(|(area, _)| area.contains(point))
            {
                let command = command.clone();
                if command == "plugin.panel" {
                    app.studio.bottom_plugin = true;
                    app.studio.terminal_focus = false;
                } else if let Err(e) = app.command(&command) {
                    app.error(e);
                }
                return true;
            }
            if let Some((_, index)) = self
                .studio_areas
                .terminal_tabs
                .iter()
                .find(|(area, _)| area.contains(point))
            {
                app.studio.terminal_active = *index;
                app.studio.terminal_focus = true;
                app.studio.bottom_plugin = false;
                return true;
            }
        }
        if self.studio_areas.terminal.contains(point) {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    app.studio.terminal_focus = true;
                    app.explorer_focus = false;
                }
                MouseEventKind::ScrollUp => {
                    if let Some(t) = app.studio.terminal_mut() {
                        t.scroll(3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(t) = app.studio.terminal_mut() {
                        t.scroll(-3);
                    }
                }
                _ => {}
            }
            return true;
        }
        if self.studio_areas.split_preview.contains(point) {
            match event.kind {
                MouseEventKind::ScrollUp => {
                    if app.studio.split_mermaid {
                        if let Some(d) = app.diagram.as_mut() {
                            d.pan_y = d.pan_y.saturating_sub(40);
                        }
                    } else {
                        app.doc_mut().preview_scroll = app.doc().preview_scroll.saturating_sub(3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if app.studio.split_mermaid {
                        if let Some(d) = app.diagram.as_mut() {
                            d.pan_y = d.pan_y.saturating_add(40);
                        }
                    } else {
                        app.doc_mut().preview_scroll = app.doc().preview_scroll.saturating_add(3);
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some((index, areas)) = &self.studio_areas.secondary
            && areas.editor.contains(point)
            && matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let old = app.active;
            app.active = *index;
            app.studio.split = Some(old);
            app.studio.split_right_focus = !app.studio.split_right_focus;
            self.mouse.areas = areas.clone();
        }
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            app.studio.terminal_focus = false;
        }
        false
    }
    pub(super) fn apply_theme(&self, frame: &mut Frame, app: &App) {
        let Some(theme) = self.themes.themes.get(&app.studio.config.theme) else {
            return;
        };
        let Some(background) = theme.settings.background else {
            return;
        };
        let bg = Color::Rgb(background.r, background.g, background.b);
        let foreground = theme
            .settings
            .foreground
            .map(|c| Color::Rgb(c.r, c.g, c.b))
            .unwrap_or(TEXT);
        if app.studio.config.theme == "base16-ocean.dark" {
            return;
        }
        for cell in &mut frame.buffer_mut().content {
            if cell.bg == BG || cell.bg == PANEL {
                cell.bg = bg;
            }
            if cell.fg == TEXT {
                cell.fg = foreground;
            }
        }
    }
    pub(super) fn wrapped_editor(
        &mut self,
        frame: &mut Frame,
        app: &mut App,
        numbers: Rect,
        text: Rect,
    ) {
        let width = usize::from(text.width).max(1);
        app.studio.wrap_width = width;
        let height = usize::from(text.height);
        let source_row = app.doc().position().0;
        let visual = app.doc().visual_column();
        let scroll = app.doc().scroll;
        let selection = app.doc().selection();
        let mut physical = Vec::new();
        let mut cursor = None;
        for row in scroll..self.lines.len() {
            let mut spans = vec![];
            let mut used = 0;
            let mut column = 0;
            let mut start = 0;
            let source_start = app.doc().text.line_to_char(row);
            let selected: Vec<bool> = app
                .doc()
                .line(row)
                .chars()
                .enumerate()
                .flat_map(|(index, ch)| {
                    std::iter::repeat_n(
                        selection
                            .as_ref()
                            .is_some_and(|range| range.contains(&(source_start + index))),
                        if ch == '\t' { 4 } else { 1 },
                    )
                })
                .collect();
            let mut char_offset = 0;
            for span in &self.lines[row].spans {
                for grapheme in span.content.graphemes(true) {
                    let size = grapheme.width();
                    if used + size > width && !spans.is_empty() {
                        physical.push((row, start, Line::from(std::mem::take(&mut spans))));
                        start = column;
                        used = 0;
                    }
                    let style = if selected.get(char_offset).copied().unwrap_or(false) {
                        span.style.bg(Color::Rgb(55, 83, 112))
                    } else {
                        span.style
                    };
                    spans.push(Span::styled(grapheme.to_owned(), style));
                    used += size;
                    column += size;
                    char_offset += grapheme.chars().count();
                }
            }
            physical.push((row, start, Line::from(spans)));
        }
        for (index, (row, start, line)) in physical.iter().enumerate() {
            let next_same = physical.get(index + 1).is_some_and(|next| next.0 == *row);
            if *row == source_row
                && visual >= *start
                && (visual < *start + line.width() || !next_same)
            {
                cursor = Some((index, visual - *start));
            }
        }
        if app.doc().wrap_base != scroll {
            app.doc_mut().wrap_base = scroll;
            app.doc_mut().wrap_scroll = 0;
        }
        if self.mouse.follow_cursor
            && let Some((index, _)) = cursor
        {
            if index < app.doc().wrap_scroll {
                app.doc_mut().wrap_scroll = index;
            }
            if index >= app.doc().wrap_scroll + height {
                app.doc_mut().wrap_scroll = index + 1 - height;
            }
        }
        app.doc_mut().wrap_scroll = app
            .doc()
            .wrap_scroll
            .min(physical.len().saturating_sub(height));
        let offset = app.doc().wrap_scroll;
        let mut lines = vec![];
        let mut gutter = vec![];
        for (row, start, line) in physical.iter().skip(offset).take(height) {
            self.mouse.areas.wrap_rows.push((*row, *start));
            gutter.push(Line::from(if *start == 0 {
                format!(
                    "{:>width$} ",
                    row + 1,
                    width = usize::from(numbers.width.saturating_sub(1))
                )
            } else {
                "↪".into()
            }));
            lines.push(line.clone());
        }
        frame.render_widget(
            Paragraph::new(gutter).style(Style::default().fg(MUTED)),
            numbers,
        );
        frame.render_widget(Paragraph::new(lines), text);
        if !app.explorer_focus
            && !app.studio.terminal_focus
            && app.dialog.is_none()
            && let Some((index, column)) = cursor
            && index >= offset
            && index < offset + height
        {
            frame.set_cursor_position((
                text.x + column.min(width.saturating_sub(1)) as u16,
                text.y + (index - offset) as u16,
            ));
        }
    }
}
fn terminal_color(color: vt100::Color, default: Color) -> Color {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::{fs, path::PathBuf};
    fn mouse_event(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }
    #[test]
    fn activity_bar_opens_localized_plugin_commands() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled/formatter");
        let mut app = App::new(root.path().into(), Language::Ru, vec![plugin])?;
        let formatter = app
            .plugins
            .plugins
            .iter()
            .position(|plugin| plugin.manifest.name == "formatter")
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 24))?;
        let mut renderer = Renderer::new();
        terminal.draw(|frame| renderer.draw(frame, &mut app))?;
        let button = renderer
            .studio_areas
            .activity_plugins
            .iter()
            .find(|(_, index)| *index == formatter)
            .unwrap()
            .0;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((button.x + 2, button.y)).unwrap().symbol(), "F");
        assert_eq!(buffer.cell((button.x + 3, button.y)).unwrap().symbol(), "M");
        assert_eq!(buffer.cell((button.x + 5, button.y)).unwrap().symbol(), "│");
        assert_eq!(button.height, 2);
        renderer.mouse(
            &mut app,
            mouse_event(
                MouseEventKind::Down(MouseButton::Left),
                button.x + 2,
                button.y + 1,
            ),
        );
        assert!(matches!(app.dialog, Some(Dialog::Workbench)));
        let panel = app.studio.panel.as_ref().unwrap();
        assert_eq!(panel.title, "formatter");
        assert!(panel.items[0].label.contains("Форматировать"));
        assert_eq!(panel.items.len(), 3);
        Ok(())
    }
    #[test]
    fn panel_drags_resize_clamp_and_preserve_document_focus() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let mut app = App::new(root.path().into(), Language::En, vec![])?;
        app.paste("source 🦀\nsecond line\n");
        app.doc_mut().anchor = Some(0);
        let before = (
            app.doc().text.to_string(),
            app.doc().cursor,
            app.doc().anchor,
        );
        app.command("split.next")?;
        app.studio.bottom_visible = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 35))?;
        let mut renderer = Renderer::new();
        terminal.draw(|f| renderer.draw(f, &mut app))?;
        for kind in [
            ResizeKind::Explorer,
            ResizeKind::Split,
            ResizeKind::Terminal,
        ] {
            let region = *renderer
                .studio_areas
                .resize
                .iter()
                .find(|r| r.kind == kind)
                .unwrap();
            let x = region.hit.x;
            let y = region.hit.y + region.hit.height / 2;
            renderer.mouse(
                &mut app,
                mouse_event(MouseEventKind::Down(MouseButton::Left), x, y),
            );
            let (dx, dy) = if kind == ResizeKind::Terminal {
                (x, y - 3)
            } else {
                (x + 8, y)
            };
            renderer.mouse(
                &mut app,
                mouse_event(MouseEventKind::Drag(MouseButton::Left), dx, dy),
            );
            // A new frame must not lose the drag, even though its border moved.
            terminal.draw(|f| renderer.draw(f, &mut app))?;
            let changed = *renderer
                .studio_areas
                .resize
                .iter()
                .find(|r| r.kind == kind)
                .unwrap();
            assert!(changed.size > region.size);
            renderer.mouse(
                &mut app,
                mouse_event(MouseEventKind::Drag(MouseButton::Left), 0, 0),
            );
            terminal.draw(|f| renderer.draw(f, &mut app))?;
            let limited = *renderer
                .studio_areas
                .resize
                .iter()
                .find(|r| r.kind == kind)
                .unwrap();
            if kind == ResizeKind::Terminal {
                assert_eq!(limited.size, limited.bounds.height - 5);
            } else {
                assert_eq!(limited.size, 12);
            }
            renderer.mouse(
                &mut app,
                mouse_event(MouseEventKind::Drag(MouseButton::Left), 500, 500),
            );
            terminal.draw(|f| renderer.draw(f, &mut app))?;
            let limited = *renderer
                .studio_areas
                .resize
                .iter()
                .find(|r| r.kind == kind)
                .unwrap();
            match kind {
                ResizeKind::Terminal => assert_eq!(limited.size, 5),
                ResizeKind::Explorer => assert_eq!(limited.size, limited.bounds.width - 10),
                ResizeKind::Split => assert!(limited.size <= limited.bounds.width - 12),
            }
            renderer.mouse(
                &mut app,
                mouse_event(MouseEventKind::Up(MouseButton::Left), 500, 500),
            );
            assert!(renderer.resize_drag.is_none());
            assert_eq!(
                (
                    app.doc().text.to_string(),
                    app.doc().cursor,
                    app.doc().anchor
                ),
                before
            );
            assert!(!app.studio.terminal_focus);
            assert!(!app.explorer_focus);
            app.command("layout.reset")?;
            terminal.draw(|f| renderer.draw(f, &mut app))?;
        }
        // Ordinary source selection still works after releasing a divider.
        let text = renderer.mouse.areas.text;
        renderer.mouse(
            &mut app,
            mouse_event(MouseEventKind::Down(MouseButton::Left), text.x, text.y),
        );
        renderer.mouse(
            &mut app,
            mouse_event(MouseEventKind::Drag(MouseButton::Left), text.x + 3, text.y),
        );
        renderer.mouse(
            &mut app,
            mouse_event(MouseEventKind::Up(MouseButton::Left), text.x + 3, text.y),
        );
        assert_eq!(app.doc().selection().unwrap(), 0..3);
        // A keystroke or a window resize cancels an unfinished drag.
        let region = renderer
            .studio_areas
            .resize
            .iter()
            .find(|r| r.kind == ResizeKind::Explorer)
            .unwrap();
        let (x, y) = (region.hit.x, region.hit.y);
        renderer.mouse(
            &mut app,
            mouse_event(MouseEventKind::Down(MouseButton::Left), x, y),
        );
        renderer.follow_cursor();
        assert!(renderer.resize_drag.is_none());
        renderer.mouse(
            &mut app,
            mouse_event(MouseEventKind::Down(MouseButton::Left), x, y),
        );
        let mut small = Terminal::new(TestBackend::new(35, 8))?;
        small.draw(|f| renderer.draw(f, &mut app))?;
        assert!(renderer.resize_drag.is_none());
        app.command("layout.explorer.grow")?;
        app.command("layout.split.grow")?;
        assert_eq!(app.studio.explorer_width, Some(30));
        assert_eq!(app.studio.split_percent, 55);
        app.command("layout.reset")?;
        assert_eq!(app.studio.explorer_width, None);
        assert_eq!(app.studio.split_percent, 50);
        assert_eq!(app.studio.bottom_height, 12);
        Ok(())
    }
    #[test]
    fn split_mouse_wrap_settings_and_palette() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        fs::write(&a, "first\n")?;
        fs::write(&b, "second\n")?;
        let mut app = App::new(root.path().into(), Language::Ru, vec![])?;
        app.open(&a)?;
        app.open(&b)?;
        app.command("split.next")?;
        let mut terminal = Terminal::new(TestBackend::new(120, 35))?;
        let mut renderer = Renderer::new();
        terminal.draw(|f| renderer.draw(f, &mut app))?;
        let (_, areas) = renderer.studio_areas.secondary.as_ref().unwrap();
        let text = areas.text;
        renderer.mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: text.x,
                row: text.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.doc().path.as_ref(), Some(&a.canonicalize()?));
        app.paste("edit ");
        assert!(app.doc().text.to_string().starts_with("edit first"));
        app.command("split.close")?;
        app.doc_mut().replace(&"🦀abcdef ".repeat(300));
        app.doc_mut().cursor = app.doc().text.len_chars();
        app.studio.config.wrap = true;
        terminal.draw(|f| renderer.draw(f, &mut app))?;
        assert!(app.doc().wrap_scroll > 0);
        assert!(!renderer.mouse.areas.wrap_rows.is_empty());
        let old = app.doc().cursor;
        app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.doc().cursor < old);
        app.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(matches!(app.dialog, Some(Dialog::Workbench)));
        app.paste("cargo.build");
        terminal.draw(|f| renderer.draw(f, &mut app))?;
        assert_eq!(app.studio.panel.as_ref().unwrap().filtered().len(), 1);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.command("theme:InspiredGitHub")?;
        terminal.draw(|f| renderer.draw(f, &mut app))?;
        assert_eq!(app.studio.config.theme, "InspiredGitHub");
        Ok(())
    }
    #[test]
    #[cfg(unix)]
    fn bottom_terminal_keeps_input_out_of_editor_and_runs_rust() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("src"))?;
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='reditor-task-test'\nversion='0.1.0'\nedition='2024'\n",
        )?;
        fs::write(
            root.path().join("src/main.rs"),
            "fn main(){let mut s=String::new();std::io::stdin().read_line(&mut s).unwrap();println!(\"RUN:{}:{}\", std::env::args().nth(1).unwrap(),s.trim());}\n",
        )?;
        let mut app = App::new(root.path().into(), Language::En, vec![])?;
        app.studio.run_args = "'Привет мир'".into();
        app.command("cargo.run")?;
        let mut terminal = Terminal::new(TestBackend::new(120, 35))?;
        let mut renderer = Renderer::new();
        terminal.draw(|f| renderer.draw(f, &mut app))?;
        assert!(renderer.studio_areas.terminal.height > 0);
        let old_height = renderer.studio_areas.terminal.height;
        let region = *renderer
            .studio_areas
            .resize
            .iter()
            .find(|r| r.kind == ResizeKind::Terminal)
            .unwrap();
        renderer.mouse(
            &mut app,
            mouse_event(
                MouseEventKind::Down(MouseButton::Left),
                region.hit.x,
                region.hit.y,
            ),
        );
        renderer.mouse(
            &mut app,
            mouse_event(
                MouseEventKind::Drag(MouseButton::Left),
                region.hit.x,
                region.hit.y - 4,
            ),
        );
        renderer.mouse(
            &mut app,
            mouse_event(
                MouseEventKind::Up(MouseButton::Left),
                region.hit.x,
                region.hit.y - 4,
            ),
        );
        terminal.draw(|f| renderer.draw(f, &mut app))?;
        assert_eq!(renderer.studio_areas.terminal.height, old_height + 4);
        assert!(app.studio.terminal_focus);
        assert_eq!(
            app.studio.terminals[0].parser.screen().size().0,
            renderer.studio_areas.terminal.height
        );
        app.paste("Rust input\n");
        assert_eq!(app.doc().text.to_string(), "");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.studio.terminals[0].exit.is_none() && std::time::Instant::now() < deadline {
            app.poll_studio();
            terminal.draw(|f| renderer.draw(f, &mut app))?;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.poll_studio();
        assert!(
            app.studio.terminals[0]
                .output
                .contains("RUN:Привет мир:Rust input")
        );
        assert!(app.studio.terminals[0].exit.is_some());
        let text = renderer.mouse.areas.text;
        renderer.mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: text.x,
                row: text.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!app.studio.terminal_focus);
        app.paste("editor");
        assert_eq!(app.doc().text.to_string(), "editor");
        app.command("terminal.close")?;
        assert!(!app.studio.bottom_visible);
        Ok(())
    }
}
