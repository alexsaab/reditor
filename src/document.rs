use anyhow::{Context, Result, bail};
use ropey::Rope;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use unicode_width::UnicodeWidthChar;

const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;
const HISTORY_LIMIT: usize = 200;

#[derive(Clone)]
struct Snapshot {
    text: Rope,
    cursor: usize,
}

pub struct Document {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub text: Rope,
    pub cursor: usize,
    pub scroll: usize,
    pub horizontal: usize,
    pub revision: u64,
    pub anchor: Option<usize>,
    pub preview: bool,
    pub preview_scroll: usize,
    pub wrap_scroll: usize,
    pub wrap_base: usize,
    saved: Rope,
    disk_text: Option<String>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            path: None,
            text: Rope::new(),
            saved: Rope::new(),
            cursor: 0,
            scroll: 0,
            horizontal: 0,
            revision: 0,
            anchor: None,
            preview: false,
            preview_scroll: 0,
            wrap_scroll: 0,
            wrap_base: 0,
            disk_text: None,
            undo: vec![],
            redo: vec![],
        }
    }
    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .with_context(|| format!("{}", path.display()))?;
        if fs::metadata(&path)?.len() > MAX_FILE_SIZE {
            bail!("File exceeds 16 MiB");
        }
        let content = fs::read_to_string(&path).context("Only UTF-8 text files are supported")?;
        if content.contains('\0') {
            bail!("Binary file");
        }
        let text = Rope::from_str(&content);
        Ok(Self {
            path: Some(path),
            saved: text.clone(),
            text,
            disk_text: Some(content),
            ..Self::new()
        })
    }
    pub fn dirty(&self) -> bool {
        self.text != self.saved
    }
    pub fn disk_baseline(&self) -> Option<&str> {
        self.disk_text.as_deref()
    }
    pub fn restore_buffer(
        &mut self,
        path: Option<PathBuf>,
        text: String,
        baseline: Option<String>,
    ) {
        self.path = path;
        self.disk_text = baseline.clone();
        self.saved = Rope::from_str(baseline.as_deref().unwrap_or(""));
        self.replace(&text);
    }
    pub fn can_preview(&self) -> bool {
        self.path.as_ref().is_none_or(|p| {
            p.extension().and_then(|s| s.to_str()).is_some_and(|e| {
                matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown")
            })
        })
    }
    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "∅".into())
    }
    fn checkpoint(&mut self) {
        self.undo.push(Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision += 1;
    }
    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.checkpoint();
        self.remove_selected();
        self.text.insert(self.cursor, text);
        self.cursor += text.chars().count();
    }
    pub fn replace(&mut self, text: &str) {
        if self.text == text {
            return;
        }
        self.checkpoint();
        self.anchor = None;
        self.text = Rope::from_str(text);
        self.cursor = self.cursor.min(self.text.len_chars());
    }
    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            self.checkpoint();
            self.remove_selected();
            return;
        }
        if self.cursor > 0 {
            self.checkpoint();
            let end = self.cursor;
            self.cursor -= 1;
            if self.text.char(self.cursor) == '\n'
                && self.cursor > 0
                && self.text.char(self.cursor - 1) == '\r'
            {
                self.cursor -= 1;
            }
            self.text.remove(self.cursor..end);
        }
    }
    pub fn delete(&mut self) {
        if self.selection().is_some() {
            self.checkpoint();
            self.remove_selected();
            return;
        }
        if self.cursor < self.text.len_chars() {
            self.checkpoint();
            let mut end = self.cursor + 1;
            if self.text.char(self.cursor) == '\r'
                && end < self.text.len_chars()
                && self.text.char(end) == '\n'
            {
                end += 1;
            }
            self.text.remove(self.cursor..end);
        }
    }
    pub fn position(&self) -> (usize, usize) {
        let row = self.text.char_to_line(self.cursor);
        (row, self.cursor - self.text.line_to_char(row))
    }
    pub fn line(&self, row: usize) -> String {
        self.text
            .line(row)
            .to_string()
            .trim_end_matches(['\r', '\n'])
            .to_owned()
    }
    pub fn visual_column(&self) -> usize {
        let (row, col) = self.position();
        self.line(row)
            .chars()
            .take(col)
            .map(|c| if c == '\t' { 4 } else { c.width().unwrap_or(0) })
            .sum()
    }
    pub fn move_vertical(&mut self, delta: isize) {
        let (row, col) = self.position();
        let target = row
            .saturating_add_signed(delta)
            .min(self.text.len_lines() - 1);
        self.cursor = self.text.line_to_char(target) + col.min(self.line(target).chars().count());
    }
    /// Convert terminal cells to a character offset, respecting tabs and wide Unicode.
    pub fn cursor_at(&self, row: usize, visual_column: usize) -> usize {
        let row = row.min(self.text.len_lines() - 1);
        let mut width = 0;
        let mut column = 0;
        for ch in self.line(row).chars() {
            let advance = if ch == '\t' {
                4
            } else {
                ch.width().unwrap_or(0)
            };
            if width + advance > visual_column {
                break;
            }
            width += advance;
            column += 1;
        }
        self.text.line_to_char(row) + column
    }
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        if self.cursor > 0
            && self.cursor < self.text.len_chars()
            && self.text.char(self.cursor) == '\n'
            && self.text.char(self.cursor - 1) == '\r'
        {
            self.cursor -= 1;
        }
    }
    pub fn right(&mut self) {
        if self.cursor < self.text.len_chars() {
            let c = self.text.char(self.cursor);
            self.cursor += 1;
            if c == '\r'
                && self.cursor < self.text.len_chars()
                && self.text.char(self.cursor) == '\n'
            {
                self.cursor += 1;
            }
        }
    }
    pub fn home(&mut self) {
        self.cursor = self.text.line_to_char(self.position().0);
    }
    pub fn end(&mut self) {
        let row = self.position().0;
        self.cursor = self.text.line_to_char(row) + self.line(row).chars().count();
    }
    pub fn newline(&mut self) {
        let row = self.position().0;
        let indent: String = self
            .line(row)
            .chars()
            .take(self.position().1)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let eol = if self.text.to_string().contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        self.insert(&format!("{eol}{indent}"));
    }
    pub fn undo(&mut self) {
        self.anchor = None;
        if let Some(s) = self.undo.pop() {
            self.redo.push(Snapshot {
                text: self.text.clone(),
                cursor: self.cursor,
            });
            self.text = s.text;
            self.cursor = s.cursor;
            self.revision += 1;
        }
    }
    pub fn redo(&mut self) {
        self.anchor = None;
        if let Some(s) = self.redo.pop() {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                cursor: self.cursor,
            });
            self.text = s.text;
            self.cursor = s.cursor;
            self.revision += 1;
        }
    }
    pub fn find_next(&mut self, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        let text = self.text.to_string();
        let start = self
            .text
            .char_to_byte((self.cursor + 1).min(self.text.len_chars()));
        let found = text[start..]
            .find(needle)
            .map(|n| n + start)
            .or_else(|| text.find(needle));
        if let Some(byte) = found {
            self.cursor = self.text.byte_to_char(byte);
            self.anchor = Some(self.cursor + needle.chars().count());
            true
        } else {
            false
        }
    }
    pub fn save(&mut self, path: &Path, allow_overwrite: bool) -> Result<()> {
        let target = if path.exists() {
            path.canonicalize()?
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            parent
                .canonicalize()?
                .join(path.file_name().context("Invalid file name")?)
        };
        if target.is_dir() {
            bail!("Path is a directory");
        }
        let existed = target.exists();
        if existed {
            if self.path.as_ref() == Some(&target) {
                let disk = fs::read_to_string(&target)?;
                if self.disk_text.as_deref() != Some(&disk) {
                    bail!("File changed on disk; reopen it or save under a different name");
                }
            } else if !allow_overwrite {
                bail!("File already exists");
            }
        }
        let parent = target.parent().context("Missing parent directory")?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        if target.exists() {
            temp.as_file()
                .set_permissions(fs::metadata(&target)?.permissions())?;
        }
        let content = self.text.to_string();
        temp.write_all(content.as_bytes())?;
        temp.as_file().sync_all()?;
        if existed {
            temp.persist(&target).map_err(|e| e.error)?;
        } else {
            temp.persist_noclobber(&target).map_err(|e| e.error)?;
        }
        self.path = Some(target);
        self.saved = self.text.clone();
        self.disk_text = Some(content);
        Ok(())
    }
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.anchor
            .filter(|&a| a != self.cursor)
            .map(|a| a.min(self.cursor)..a.max(self.cursor))
    }
    pub fn selected_text(&self) -> Option<String> {
        self.selection()
            .map(|range| self.text.slice(range).to_string())
    }
    fn remove_selected(&mut self) {
        if let Some(range) = self.selection() {
            self.cursor = range.start;
            self.text.remove(range);
        }
        self.anchor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_edit_undo_and_search() {
        let mut d = Document::new();
        d.insert("привет 🦀\n世界");
        d.backspace();
        assert_eq!(d.text.to_string(), "привет 🦀\n世");
        d.undo();
        assert_eq!(d.text.to_string(), "привет 🦀\n世界");
        d.redo();
        assert!(d.find_next("🦀"));
        assert_eq!(d.position(), (0, 7));
        d.move_vertical(1);
        assert_eq!(d.position(), (1, 1));
    }
    #[test]
    fn saving_conflicts_and_dirty_history() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("hello.rs");
        let mut d = Document::new();
        d.insert("fn main() {}\n");
        d.save(&path, false)?;
        assert!(!d.dirty());
        d.insert("// edit");
        assert!(d.dirty());
        d.undo();
        assert!(!d.dirty());
        fs::write(&path, "external")?;
        assert!(d.save(&path, false).is_err());
        assert_eq!(fs::read_to_string(&path)?, "external");
        Ok(())
    }
    #[test]
    fn crlf_editing() {
        let mut d = Document::new();
        d.insert("a\r\nb");
        d.cursor = 1;
        d.right();
        assert_eq!(d.cursor, 3);
        d.backspace();
        assert_eq!(d.text.to_string(), "ab");
        d.undo();
        d.cursor = 1;
        d.delete();
        assert_eq!(d.text.to_string(), "ab");
    }
    #[test]
    fn selection_replacement_and_undo() {
        let mut d = Document::new();
        d.insert("a🦀приветz");
        d.anchor = Some(1);
        d.cursor = 8;
        assert_eq!(d.selected_text().as_deref(), Some("🦀привет"));
        d.insert("世界");
        assert_eq!(d.text.to_string(), "a世界z");
        assert!(d.anchor.is_none());
        d.undo();
        assert_eq!(d.text.to_string(), "a🦀приветz");
        d.anchor = Some(0);
        d.cursor = d.text.len_chars();
        d.delete();
        assert_eq!(d.text.len_chars(), 0);
        d.undo();
        assert_eq!(d.text.to_string(), "a🦀приветz");
    }
}
