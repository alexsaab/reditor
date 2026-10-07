use crate::app::{App, Dialog};
use crate::i18n::Language;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Default)]
pub struct HitAreas {
    pub menu_headers: Vec<(Rect, usize)>,
    pub toolbar: Vec<(Rect, KeyEvent)>,
    pub tabs: Vec<(Rect, usize)>,
    pub explorer: Rect,
    pub parent: Rect,
    pub files: Rect,
    pub editor: Rect,
    pub mode_toggle: Rect,
    pub preview_rows: usize,
    pub text: Rect,
    pub dialog: Rect,
    pub dialog_content: Rect,
    pub confirm: Rect,
    pub cancel: Rect,
    pub dialog_close: Rect,
    pub plugin_offset: usize,
    pub diagram_actions: Vec<(Rect, KeyCode)>,
    pub wrap_rows: Vec<(usize, usize)>,
}

pub struct MouseController {
    pub areas: HitAreas,
    pub follow_cursor: bool,
    dragging: Option<u64>,
    last_click: Option<(String, Instant)>,
}
impl Default for MouseController {
    fn default() -> Self {
        Self {
            areas: HitAreas::default(),
            follow_cursor: true,
            dragging: None,
            last_click: None,
        }
    }
}

impl MouseController {
    pub fn keyboard(&mut self) {
        self.follow_cursor = true;
        self.dragging = None;
        self.last_click = None;
    }
    fn key(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
    }
    fn double_click(&mut self, target: String) -> bool {
        let now = Instant::now();
        let double = self.last_click.as_ref().is_some_and(|(old, time)| {
            old == &target && now.duration_since(*time) < Duration::from_millis(450)
        });
        self.last_click = if double { None } else { Some((target, now)) };
        double
    }
    pub fn handle(&mut self, app: &mut App, event: MouseEvent) {
        let point = Position::new(event.column, event.row);
        if matches!(event.kind, MouseEventKind::Up(MouseButton::Left)) {
            self.dragging = None;
            return;
        }
        if app.dialog.is_some() {
            self.dialog(app, event, point);
            return;
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.dragging = None;
                if let Some((_, menu)) = self
                    .areas
                    .menu_headers
                    .iter()
                    .find(|(area, _)| area.contains(point))
                {
                    self.follow_cursor = true;
                    app.dialog = Some(Dialog::Menu {
                        menu: *menu,
                        selected: 0,
                    });
                } else if let Some((_, key)) = self
                    .areas
                    .toolbar
                    .iter()
                    .find(|(area, _)| area.contains(point))
                {
                    self.follow_cursor = true;
                    app.key(*key);
                } else if self.areas.mode_toggle.contains(point) {
                    self.follow_cursor = true;
                    Self::key(app, KeyCode::F(7));
                } else if let Some((_, index)) = self
                    .areas
                    .tabs
                    .iter()
                    .find(|(area, _)| area.contains(point))
                {
                    app.active = *index;
                    app.explorer_focus = false;
                    self.follow_cursor = true;
                    self.last_click = None;
                } else if self.areas.parent.contains(point) {
                    app.explorer_focus = true;
                    Self::key(app, KeyCode::Backspace);
                } else if self.areas.explorer.contains(point) {
                    app.explorer_focus = true;
                    if self.areas.files.contains(point) {
                        let index = app.explorer_offset + (event.row - self.areas.files.y) as usize;
                        if let Some(entry) = app.entries.get(index) {
                            let path = entry.path.clone();
                            app.selected = index;
                            if self.double_click(format!("file:{}", path.display())) {
                                Self::key(app, KeyCode::Enter);
                                self.follow_cursor = true;
                            }
                        }
                    }
                } else if self.areas.editor.contains(point) {
                    app.explorer_focus = false;
                    if app.doc().preview {
                        self.dragging = None;
                        return;
                    }
                    self.follow_cursor = true;
                    let mut row =
                        app.doc().scroll + event.row.saturating_sub(self.areas.text.y) as usize;
                    let mut col = app.doc().horizontal
                        + event.column.saturating_sub(self.areas.text.x) as usize;
                    if let Some((source, start)) = self
                        .areas
                        .wrap_rows
                        .get(event.row.saturating_sub(self.areas.text.y) as usize)
                    {
                        row = *source;
                        col = *start + event.column.saturating_sub(self.areas.text.x) as usize;
                    }
                    let cursor = app.doc().cursor_at(row, col);
                    let old_cursor = app.doc().cursor;
                    if event.modifiers.contains(KeyModifiers::SHIFT) {
                        app.doc_mut().anchor.get_or_insert(old_cursor);
                        app.doc_mut().cursor = cursor;
                    } else if event.column < self.areas.text.x {
                        let row = row.min(app.doc().text.len_lines() - 1);
                        app.doc_mut().anchor = Some(app.doc().text.line_to_char(row));
                        app.doc_mut().cursor = if row + 1 < app.doc().text.len_lines() {
                            app.doc().text.line_to_char(row + 1)
                        } else {
                            app.doc().text.len_chars()
                        };
                    } else if self.double_click(format!("text:{}:{cursor}", app.doc().id)) {
                        let row = row.min(app.doc().text.len_lines() - 1);
                        let start = app.doc().text.line_to_char(row);
                        let chars: Vec<char> = app.doc().line(row).chars().collect();
                        let column = cursor - start;
                        let mut left = column;
                        let mut right = column;
                        if let Some(ch) = chars.get(column) {
                            let word = |c: char| c.is_alphanumeric() || c == '_';
                            if word(*ch) {
                                while left > 0 && word(chars[left - 1]) {
                                    left -= 1;
                                }
                                while right < chars.len() && word(chars[right]) {
                                    right += 1;
                                }
                            } else {
                                right += 1;
                            }
                        }
                        app.doc_mut().anchor = Some(start + left);
                        app.doc_mut().cursor = start + right;
                    } else {
                        app.doc_mut().cursor = cursor;
                        app.doc_mut().anchor = Some(cursor);
                    }
                    self.dragging = Some(app.doc().id);
                }
            }
            MouseEventKind::Drag(MouseButton::Left)
                if self.dragging == Some(app.doc().id) && self.areas.text.height > 0 =>
            {
                self.follow_cursor = false;
                if event.row < self.areas.text.y {
                    app.doc_mut().scroll = app.doc().scroll.saturating_sub(1);
                }
                if event.row >= self.areas.text.bottom() {
                    app.doc_mut().scroll = (app.doc().scroll + 1).min(
                        app.doc()
                            .text
                            .len_lines()
                            .saturating_sub(self.areas.text.height as usize),
                    );
                }
                if event.column >= self.areas.text.right() {
                    app.doc_mut().horizontal = app
                        .doc()
                        .horizontal
                        .saturating_add(1)
                        .min(u16::MAX as usize);
                }
                if event.column < self.areas.text.x {
                    app.doc_mut().horizontal = app.doc().horizontal.saturating_sub(1);
                }
                let y = event
                    .row
                    .clamp(self.areas.text.y, self.areas.text.bottom() - 1);
                let x = event
                    .column
                    .clamp(self.areas.text.x, self.areas.text.right() - 1);
                let (row, column) = if let Some((source, start)) =
                    self.areas.wrap_rows.get((y - self.areas.text.y) as usize)
                {
                    (*source, *start + (x - self.areas.text.x) as usize)
                } else {
                    (
                        app.doc().scroll + (y - self.areas.text.y) as usize,
                        app.doc().horizontal + (x - self.areas.text.x) as usize,
                    )
                };
                app.doc_mut().cursor = app.doc().cursor_at(row, column);
            }
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                let up = matches!(
                    event.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollLeft
                );
                if self.areas.editor.contains(point) {
                    if app.doc().preview {
                        let max = self
                            .areas
                            .preview_rows
                            .saturating_sub(self.areas.text.height as usize);
                        app.doc_mut().preview_scroll = if up {
                            app.doc().preview_scroll.saturating_sub(3)
                        } else {
                            app.doc().preview_scroll.saturating_add(3).min(max)
                        };
                        return;
                    }
                    self.follow_cursor = false;
                    let horizontal = event.modifiers.contains(KeyModifiers::SHIFT)
                        || matches!(
                            event.kind,
                            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                        );
                    if horizontal {
                        let end = (app.doc().scroll + self.areas.text.height as usize)
                            .min(app.doc().text.len_lines());
                        let max = (app.doc().scroll..end)
                            .map(|r| app.doc().line(r).replace('\t', "    ").width())
                            .max()
                            .unwrap_or(0)
                            .saturating_sub(self.areas.text.width as usize);
                        app.doc_mut().horizontal = if up {
                            app.doc().horizontal.saturating_sub(3)
                        } else {
                            (app.doc().horizontal + 3).min(max)
                        };
                    } else {
                        let max = app
                            .doc()
                            .text
                            .len_lines()
                            .saturating_sub(self.areas.text.height as usize);
                        app.doc_mut().scroll = if up {
                            app.doc().scroll.saturating_sub(3)
                        } else {
                            (app.doc().scroll + 3).min(max)
                        };
                    }
                } else if self.areas.explorer.contains(point) {
                    let height = self.areas.files.height.max(1) as usize;
                    let max = app.entries.len().saturating_sub(height);
                    app.explorer_offset = if up {
                        app.explorer_offset.saturating_sub(3).min(max)
                    } else {
                        (app.explorer_offset + 3).min(max)
                    };
                    app.selected = app.selected.clamp(
                        app.explorer_offset,
                        (app.explorer_offset + height - 1).min(app.entries.len().saturating_sub(1)),
                    );
                }
            }
            MouseEventKind::Down(MouseButton::Middle) if self.areas.editor.contains(point) => {
                app.explorer_focus = false;
                self.follow_cursor = true;
                app.key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
            }
            _ => {}
        }
    }
    fn dialog(&mut self, app: &mut App, event: MouseEvent, point: Position) {
        if matches!(app.dialog, Some(Dialog::Diagram)) {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if self.areas.dialog_close.contains(point) {
                        Self::key(app, KeyCode::Esc);
                    } else if let Some((_, code)) = self
                        .areas
                        .diagram_actions
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        Self::key(app, *code);
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    if self.areas.dialog.contains(point) =>
                {
                    let up = event.kind == MouseEventKind::ScrollUp;
                    Self::key(
                        app,
                        if event.modifiers.contains(KeyModifiers::CONTROL) {
                            KeyCode::Char(if up { '+' } else { '-' })
                        } else if event.modifiers.contains(KeyModifiers::SHIFT) {
                            if up { KeyCode::Left } else { KeyCode::Right }
                        } else {
                            if up { KeyCode::Up } else { KeyCode::Down }
                        },
                    );
                }
                MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                    if self.areas.dialog.contains(point) =>
                {
                    Self::key(
                        app,
                        if event.kind == MouseEventKind::ScrollLeft {
                            KeyCode::Left
                        } else {
                            KeyCode::Right
                        },
                    );
                }
                _ => {}
            }
            return;
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.areas.dialog_close.contains(point) || self.areas.cancel.contains(point) {
                    Self::key(app, KeyCode::Esc);
                } else if self.areas.confirm.contains(point) {
                    self.follow_cursor = true;
                    Self::key(app, KeyCode::Enter);
                } else if self.areas.dialog_content.contains(point)
                    && matches!(app.dialog, Some(Dialog::Language { .. }))
                {
                    let selected =
                        self.areas.plugin_offset + (point.y - self.areas.dialog_content.y) as usize;
                    if selected < Language::ALL.len() {
                        app.dialog = Some(Dialog::Language { selected });
                        Self::key(app, KeyCode::Enter);
                    }
                } else if self.areas.dialog_content.contains(point)
                    && matches!(app.dialog, Some(Dialog::Plugins { .. }))
                {
                    let selected =
                        self.areas.plugin_offset + (point.y - self.areas.dialog_content.y) as usize;
                    if selected < app.plugins.commands().len() {
                        app.dialog = Some(Dialog::Plugins { selected });
                        if self.double_click(format!("plugin:{selected}")) {
                            self.follow_cursor = true;
                            Self::key(app, KeyCode::Enter);
                        }
                    } else if self.areas.dialog_content.contains(point)
                        && let Some(Dialog::Menu { menu, .. }) = app.dialog.as_ref()
                    {
                        let selected = (point.y - self.areas.dialog_content.y) as usize;
                        if selected < crate::app::MENU_ITEMS[*menu].len() {
                            app.dialog = Some(Dialog::Menu {
                                menu: *menu,
                                selected,
                            });
                            Self::key(app, KeyCode::Enter);
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.areas.dialog.contains(point) =>
            {
                for _ in 0..3 {
                    Self::key(
                        app,
                        if event.kind == MouseEventKind::ScrollUp {
                            KeyCode::Up
                        } else {
                            KeyCode::Down
                        },
                    );
                }
            }
            _ => {}
        }
    }
}
