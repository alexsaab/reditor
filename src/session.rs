use crate::document::Document;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub active: usize,
    pub tabs: Vec<Tab>,
}
#[derive(Serialize, Deserialize)]
pub struct Tab {
    path: Option<PathBuf>,
    text: Option<String>,
    baseline: Option<String>,
    cursor: usize,
    scroll: usize,
    horizontal: usize,
    preview: bool,
    preview_scroll: usize,
    #[serde(default)]
    wrap_scroll: usize,
}
pub fn save(root: &Path, documents: &[Document], active: usize) -> Result<()> {
    let directory = root.join(".reditor");
    fs::create_dir_all(&directory)?;
    let tabs = documents
        .iter()
        .map(|d| Tab {
            path: d.path.clone(),
            text: d.dirty().then(|| d.text.to_string()),
            baseline: if d.dirty() {
                d.disk_baseline().map(str::to_owned)
            } else {
                None
            },
            cursor: d.cursor,
            scroll: d.scroll,
            horizontal: d.horizontal,
            preview: d.preview,
            preview_scroll: d.preview_scroll,
            wrap_scroll: d.wrap_scroll,
        })
        .collect();
    let value = Session { active, tabs };
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    temp.write_all(&serde_json::to_vec(&value)?)?;
    temp.as_file().sync_all()?;
    temp.persist(root.join(".reditor/session.json"))?;
    Ok(())
}
pub fn load(root: &Path) -> Result<(Vec<Document>, usize)> {
    let path = root.join(".reditor/session.json");
    if !path.exists() {
        return Ok((vec![], 0));
    }
    anyhow::ensure!(
        fs::metadata(&path)?.len() <= 128 * 1024 * 1024,
        "Session exceeds 128 MiB"
    );
    let session: Session = serde_json::from_slice(&fs::read(path)?)?;
    let mut documents = vec![];
    for tab in session.tabs {
        let mut document = match tab.path.as_deref() {
            Some(path) => Document::open(path).unwrap_or_default(),
            None => Document::new(),
        };
        if let Some(text) = tab.text {
            document.restore_buffer(tab.path.clone(), text, tab.baseline);
        } else if tab.path.is_some() && document.path.is_none() {
            continue;
        }
        document.cursor = tab.cursor.min(document.text.len_chars());
        document.scroll = tab.scroll.min(document.text.len_lines().saturating_sub(1));
        document.horizontal = tab.horizontal;
        document.preview = tab.preview;
        document.preview_scroll = tab.preview_scroll;
        document.wrap_scroll = tab.wrap_scroll;
        documents.push(document);
    }
    let active = session.active.min(documents.len().saturating_sub(1));
    Ok((documents, active))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_preserves_unsaved_text_and_external_conflict() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("main.rs");
        fs::write(&path, "old")?;
        let mut d = Document::open(&path)?;
        d.replace("unsaved 🦀");
        d.cursor = 4;
        save(root.path(), &[d], 0)?;
        fs::write(&path, "external")?;
        let (mut tabs, _) = load(root.path())?;
        assert_eq!(tabs[0].text.to_string(), "unsaved 🦀");
        assert_eq!(tabs[0].cursor, 4);
        assert!(tabs[0].dirty());
        assert!(tabs[0].save(&path, false).is_err());
        assert_eq!(fs::read_to_string(path)?, "external");
        Ok(())
    }
}
