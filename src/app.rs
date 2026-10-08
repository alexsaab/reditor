use crate::{
    diagram::{self, DiagramView},
    document::Document,
    i18n::{I18n, Language},
    plugins::{PluginInput, PluginManager},
    process,
};
use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub language: Language,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_indent")]
    pub indent: usize,
    #[serde(default)]
    pub wrap: bool,
    #[serde(default = "default_lsp")]
    pub rust_analyzer: String,
    #[serde(default)]
    pub keybindings: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub plugin_settings: std::collections::BTreeMap<String, serde_json::Value>,
}
fn default_theme() -> String {
    "base16-ocean.dark".into()
}
fn default_indent() -> usize {
    4
}
fn default_lsp() -> String {
    "rust-analyzer".into()
}
impl Default for Config {
    fn default() -> Self {
        Self {
            language: Language::default(),
            theme: default_theme(),
            indent: 4,
            wrap: false,
            rust_analyzer: default_lsp(),
            keybindings: Default::default(),
            plugin_settings: Default::default(),
        }
    }
}
#[derive(Clone, Copy)]
pub enum Prompt {
    Open,
    SaveAs,
    NewFile,
    NewDir,
    RenameFile,
    Search,
}
impl Prompt {
    pub fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::SaveAs => "save_as",
            Self::NewFile => "new_file",
            Self::NewDir => "new_dir",
            Self::RenameFile => "rename_file",
            Self::Search => "search",
        }
    }
}
pub enum Confirmation {
    Quit,
    Close,
    Overwrite(PathBuf),
    Delete(PathBuf),
}
pub enum Dialog {
    Prompt {
        kind: Prompt,
        input: String,
    },
    Confirm(Confirmation),
    Help {
        scroll: u16,
    },
    Plugins {
        selected: usize,
    },
    Language {
        selected: usize,
    },
    Output {
        scroll: u16,
    },
    Diagram,
    Workbench,
    Menu {
        menu: usize,
        selected: usize,
    },
    ExplorerContext {
        path: PathBuf,
        selected: usize,
        x: u16,
        y: u16,
    },
    FolderPicker {
        directory: PathBuf,
        entries: Vec<Entry>,
        selected: usize,
        offset: usize,
    },
}

pub const MENU_NAMES: [&str; 9] = [
    "menu_file",
    "menu_edit",
    "menu_selection",
    "menu_view",
    "menu_go",
    "menu_run",
    "menu_terminal",
    "menu_help",
    "menu_language",
];
pub const EXPLORER_CONTEXT_ITEMS: [&str; 5] = [
    "context_open",
    "context_rename",
    "context_delete",
    "context_copy_absolute_path",
    "context_copy_relative_path",
];
pub const MENU_ITEMS: [&[(&str, &str, &str)]; 9] = [
    &[
        ("menu_new_file", "Ctrl+N", "new"),
        ("menu_new_folder", "Ctrl+D", "new_dir"),
        ("menu_open_file_or_folder", "Ctrl+O", "open"),
        ("menu_open_folder", "", "open_folder"),
        ("recent_projects", "Ctrl+Alt+P", "recent_projects"),
        ("menu_save", "Ctrl+S", "save"),
        ("menu_save_as", "F4", "save_as"),
        ("menu_close_editor", "Ctrl+W", "close"),
        ("menu_exit", "Ctrl+Q", "quit"),
    ],
    &[
        ("menu_undo", "Ctrl+Z", "undo"),
        ("menu_redo", "Ctrl+Y", "redo"),
        ("menu_cut", "Ctrl+X", "cut"),
        ("menu_copy", "Ctrl+C", "copy"),
        ("menu_paste", "Ctrl+V", "paste"),
        ("menu_find", "Ctrl+F", "find"),
        ("menu_format_document", "Ctrl+R", "format"),
    ],
    &[("menu_select_all", "Ctrl+A", "select_all")],
    &[
        ("menu_explorer", "Ctrl+E", "explorer"),
        ("menu_toggle_markdown_preview", "F7", "preview"),
        ("menu_output", "F8", "output"),
        ("menu_terminal", "F10", "terminal"),
    ],
    &[
        ("menu_next_match", "F3", "next_match"),
        ("menu_next_editor", "F6", "next_tab"),
        ("menu_previous_editor", "Ctrl+Tab", "previous_tab"),
    ],
    &[
        ("menu_build_workspace", "Ctrl+B", "build"),
        ("menu_run_rust_project", "F11", "run"),
        ("menu_command_palette", "Ctrl+Shift+P", "palette"),
    ],
    &[("menu_toggle_terminal", "F10", "terminal")],
    &[
        ("menu_keyboard_shortcuts_help", "F1", "help"),
        ("menu_plugins", "Ctrl+Shift+P", "palette"),
    ],
    &[("menu_change_language", "", "language")],
];
#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub directory: bool,
}
pub struct App {
    pub root: PathBuf,
    pub directory: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub explorer_offset: usize,
    pub explorer_focus: bool,
    pub documents: Vec<Document>,
    pub active: usize,
    pub i18n: I18n,
    pub status: String,
    pub status_error: bool,
    pub dialog: Option<Dialog>,
    pub plugins: PluginManager,
    pub plugin_dirs: Vec<PathBuf>,
    pub search: String,
    pub diagram: Option<DiagramView>,
    pub studio: crate::studio::Studio,
    clipboard: String,
    system_clipboard: Option<arboard::Clipboard>,
    pub quit: bool,
    pub build_output: String,
    pub build: Option<Receiver<Result<(bool, String, String)>>>,
    build_worker: Option<thread::JoinHandle<()>>,
    build_cancel: Arc<AtomicBool>,
}

fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in &from_components[common..] {
        match component {
            Component::Normal(_) | Component::ParentDir => relative.push(".."),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    for component in &to_components[common..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

impl App {
    pub fn new(root: PathBuf, language: Language, plugin_dirs: Vec<PathBuf>) -> Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            bail!("Workspace must be a directory");
        }
        let plugin_dirs = crate::settings::plugin_dirs(&root, &plugin_dirs)?;
        let (plugins, errors) = PluginManager::load(&plugin_dirs);
        let i18n = I18n::new(language);
        let status = i18n.t("ready").to_owned();
        let studio = crate::studio::Studio::new(&root, language)?;
        let mut app = Self {
            directory: root.clone(),
            root,
            entries: vec![],
            selected: 0,
            explorer_offset: 0,
            explorer_focus: false,
            documents: vec![Document::new()],
            active: 0,
            i18n,
            status,
            status_error: false,
            dialog: None,
            plugins,
            plugin_dirs,
            search: String::new(),
            diagram: None,
            studio,
            clipboard: String::new(),
            system_clipboard: None,
            quit: false,
            build_output: String::new(),
            build: None,
            build_worker: None,
            build_cancel: Arc::new(AtomicBool::new(false)),
        };
        app.refresh()?;
        if !errors.is_empty() {
            app.error(anyhow::anyhow!(errors.join("; ")));
        }
        Ok(app)
    }
    pub fn doc(&self) -> &Document {
        &self.documents[self.active]
    }
    pub fn doc_mut(&mut self) -> &mut Document {
        &mut self.documents[self.active]
    }
    pub fn message(&mut self, key: &str) {
        self.status = self.i18n.t(key).to_owned();
        self.status_error = false;
    }
    pub fn error(&mut self, error: anyhow::Error) {
        self.status = format!("{}: {error:#}", self.i18n.t("error"));
        self.status_error = true;
    }
    pub fn refresh(&mut self) -> Result<()> {
        let mut entries = vec![];
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let path = entry.path();
            entries.push(Entry {
                directory: path.is_dir(),
                path,
            });
        }
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
        });
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        Ok(())
    }
    fn rename_selected(&mut self, name: &str) -> Result<()> {
        let Some(entry) = self.entries.get(self.selected) else {
            return Ok(());
        };
        let source = entry.path.clone();
        let target_name = Path::new(name.trim());
        anyhow::ensure!(
            !target_name.as_os_str().is_empty()
                && target_name.file_name().is_some()
                && target_name.components().count() == 1,
            "Invalid file name"
        );
        let target = self.directory.join(target_name);
        anyhow::ensure!(target != source, "Name is unchanged");
        anyhow::ensure!(
            !target.exists(),
            "A file or directory with that name already exists"
        );
        let canonical_source = if fs::symlink_metadata(&source)?.file_type().is_symlink() {
            None
        } else {
            Some(source.canonicalize()?)
        };
        fs::rename(&source, &target)?;
        self.rewrite_document_paths(
            &source,
            canonical_source.as_deref(),
            &target.canonicalize()?,
        );
        self.refresh()?;
        self.selected = self
            .entries
            .iter()
            .position(|item| item.path == target)
            .unwrap_or(0);
        self.message("renamed");
        Ok(())
    }
    pub(crate) fn move_entry(&mut self, source: &Path, directory: &Path) -> Result<()> {
        let source = source.to_owned();
        let directory = directory.canonicalize()?;
        anyhow::ensure!(directory.is_dir(), "Destination is not a directory");
        anyhow::ensure!(source != directory, "Cannot move a directory into itself");
        anyhow::ensure!(
            !directory.starts_with(&source),
            "Cannot move a directory into one of its descendants"
        );
        let name = source.file_name().context("Invalid source path")?;
        let target = directory.join(name);
        anyhow::ensure!(
            !target.exists(),
            "Destination already contains an item with that name"
        );
        let canonical_source = if fs::symlink_metadata(&source)?.file_type().is_symlink() {
            None
        } else {
            Some(source.canonicalize()?)
        };
        fs::rename(&source, &target)?;
        self.rewrite_document_paths(
            &source,
            canonical_source.as_deref(),
            &target.canonicalize()?,
        );
        self.refresh()?;
        self.message("moved");
        Ok(())
    }
    fn rewrite_document_paths(
        &mut self,
        source: &Path,
        canonical_source: Option<&Path>,
        target: &Path,
    ) {
        for doc in &mut self.documents {
            if let Some(path) = doc.path.as_ref()
                && let Some(relative) = path
                    .strip_prefix(source)
                    .ok()
                    .or_else(|| canonical_source.and_then(|root| path.strip_prefix(root).ok()))
            {
                doc.path = Some(if relative.as_os_str().is_empty() {
                    target.to_owned()
                } else {
                    target.join(relative)
                });
            }
        }
    }
    fn delete_path(&mut self, path: &Path) -> Result<()> {
        let canonical_path = if fs::symlink_metadata(path)?.file_type().is_symlink() {
            None
        } else {
            Some(path.canonicalize()?)
        };
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
        for doc in &mut self.documents {
            if doc.path.as_ref().is_some_and(|document_path| {
                document_path.starts_with(path)
                    || canonical_path
                        .as_ref()
                        .is_some_and(|root| document_path.starts_with(root))
            }) {
                doc.path = None;
            }
        }
        self.refresh()?;
        self.message("deleted");
        Ok(())
    }
    fn resolve(&self, value: &str) -> PathBuf {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            self.directory.join(path)
        }
    }
    pub fn open(&mut self, path: &Path) -> Result<()> {
        let canonical = path.canonicalize()?;
        if let Some(index) = self
            .documents
            .iter()
            .position(|d| d.path.as_ref() == Some(&canonical))
        {
            self.active = index;
            self.explorer_focus = false;
            return Ok(());
        }
        let mut document = Document::open(&canonical)?;
        let text = self.hooks(
            "on_open",
            &document.text.to_string(),
            document.path.as_deref(),
        )?;
        document.replace(&text);
        if self.documents.len() == 1
            && self.doc().path.is_none()
            && self.doc().text.len_chars() == 0
        {
            self.documents[0] = document;
        } else {
            self.documents.push(document);
            self.active = self.documents.len() - 1;
        }
        self.explorer_focus = false;
        self.message("ready");
        Ok(())
    }
    pub fn switch_project(&mut self, path: &Path) -> Result<()> {
        let canonical = path.canonicalize()?;
        if canonical == self.root {
            return Ok(());
        }
        let user_plugins = crate::settings::user_dir()?.join("plugins");
        let project_plugins = self.root.join(".reditor/plugins");
        let explicit = self
            .plugin_dirs
            .iter()
            .filter(|path| **path != user_plugins && **path != project_plugins)
            .cloned()
            .collect();
        let mut next = Self::new(canonical, self.i18n.language, explicit)?;
        crate::session::save(&self.root, &self.documents, self.active)?;
        let (documents, active) = crate::session::load(&next.root)?;
        if !documents.is_empty() {
            next.documents = documents;
            next.active = active;
        }
        let _ = crate::settings::register_project(&next.root);
        *self = next;
        Ok(())
    }
    pub(crate) fn open_folder_picker(&mut self) -> Result<()> {
        let directory = self.directory.canonicalize()?;
        let entries = Self::folder_entries(&directory)?;
        self.dialog = Some(Dialog::FolderPicker {
            directory,
            entries,
            selected: 0,
            offset: 0,
        });
        Ok(())
    }
    fn folder_entries(directory: &Path) -> Result<Vec<Entry>> {
        let mut entries = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                path.is_dir().then_some(Entry {
                    path,
                    directory: true,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
        Ok(entries)
    }
    pub(crate) fn folder_picker_choose(&mut self) -> Result<()> {
        let Some(Dialog::FolderPicker { directory, .. }) = self.dialog.as_ref() else {
            return Ok(());
        };
        let directory = directory.clone();
        self.switch_project(&directory)?;
        self.dialog = None;
        Ok(())
    }
    fn folder_picker_enter(&mut self) -> Result<()> {
        let Some(Dialog::FolderPicker {
            directory,
            entries,
            selected,
            ..
        }) = self.dialog.as_ref()
        else {
            return Ok(());
        };
        let parent_offset = usize::from(directory.parent().is_some());
        let next = if parent_offset == 1 && *selected == 0 {
            directory.parent().map(Path::to_owned)
        } else {
            entries
                .get(selected.saturating_sub(parent_offset))
                .map(|entry| entry.path.clone())
        };
        let Some(next) = next else {
            return Ok(());
        };
        let next = next.canonicalize()?;
        let entries = Self::folder_entries(&next)?;
        self.dialog = Some(Dialog::FolderPicker {
            directory: next,
            entries,
            selected: 0,
            offset: 0,
        });
        Ok(())
    }
    pub(crate) fn hooks(&self, event: &str, text: &str, path: Option<&Path>) -> Result<String> {
        let mut text = text.to_owned();
        for (index, plugin) in self.plugins.plugins.iter().enumerate() {
            if plugin.manifest.hooks.iter().any(|h| h == event) {
                let settings = crate::settings::plugin_settings(&self.root, &plugin.manifest.name)?;
                let result = self.plugins.execute_with_settings(
                    index,
                    &PluginInput {
                        event,
                        text: &text,
                        path: path.map(|p| p.to_string_lossy().into_owned()),
                        language: &self.i18n.code().to_lowercase(),
                    },
                    &settings,
                )?;
                anyhow::ensure!(
                    result.action.is_none(),
                    "Host actions cannot run from automatic hooks"
                );
                if let Some(updated) = result.text {
                    text = updated;
                }
            }
        }
        Ok(text)
    }
    pub fn save(&mut self, path: &Path, overwrite: bool) -> Result<()> {
        let text = self.hooks("before_save", &self.doc().text.to_string(), Some(path))?;
        let text = self.formatter_on_save(path, &text)?;
        if path == self.root.join(".reditor/config.toml")
            || path == crate::settings::user_dir()?.join("config.toml")
        {
            let config: Config = toml::from_str(&text).context("Invalid editor configuration")?;
            anyhow::ensure!((1..=16).contains(&config.indent), "Indent must be 1..16");
        }
        if path == self.root.join(".reditor/connections.toml")
            || path == crate::settings::user_dir()?.join("connections.toml")
        {
            let _: toml::Value = toml::from_str(&text).context("Invalid connections.toml")?;
        }
        self.doc_mut().replace(&text);
        self.doc_mut().save(path, overwrite)?;
        if path == self.root.join(".reditor/config.toml")
            || path == crate::settings::user_dir()?.join("config.toml")
        {
            self.command("settings.reload")?;
        }
        if let Some(client) = self.studio.lsp.as_mut()
            && client.ready()
        {
            let _ = client.sync(&self.documents);
            if let Ok(uri) = crate::lsp::uri(path) {
                let _ = client.notify(
                    "textDocument/didSave",
                    serde_json::json!({ "textDocument": { "uri": uri } }),
                );
            }
        }
        self.refresh()?;
        self.message("saved");
        if self.remote_binding(path).is_some() {
            self.remote_upload_saved(
                self.doc().path.clone().context("Missing saved path")?,
                self.doc().text.to_string(),
            )?;
        }
        Ok(())
    }
    fn prompt(&mut self, kind: Prompt) {
        let input = match kind {
            Prompt::SaveAs => self
                .doc()
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Prompt::Search => self.search.clone(),
            Prompt::RenameFile => self
                .entries
                .get(self.selected)
                .and_then(|entry| entry.path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.dialog = Some(Dialog::Prompt { kind, input });
    }
    fn save_current(&mut self) -> Result<()> {
        if let Some(path) = self.doc().path.clone() {
            self.save(&path, false)
        } else {
            self.prompt(Prompt::SaveAs);
            Ok(())
        }
    }
    fn close(&mut self) {
        self.documents.remove(self.active);
        if self.documents.is_empty() {
            self.documents.push(Document::new());
        }
        self.active = self.active.min(self.documents.len() - 1);
        self.studio.split = None;
    }
    pub fn paste(&mut self, text: &str) {
        if matches!(self.dialog, Some(Dialog::Workbench)) {
            if let Some(panel) = self.studio.panel.as_mut() {
                panel.input.push_str(&text.replace(['\r', '\n'], ""));
            }
            return;
        }
        if self.dialog.is_none() && self.studio.terminal_focus {
            if let Some(terminal) = self.studio.terminal_mut()
                && let Err(e) = terminal.paste(text)
            {
                self.error(e);
            }
            return;
        }
        let preview = self.doc().preview;
        match self.dialog.as_mut() {
            Some(Dialog::Prompt { input, .. }) => {
                input.extend(text.chars().filter(|c| !c.is_control()))
            }
            None if !self.explorer_focus && preview => self.message("preview_read_only"),
            None if !self.explorer_focus => self.doc_mut().insert(&text.replace('\0', "")),
            _ => {}
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        if let Err(e) = self.handle_key(key) {
            self.error(e);
        }
    }
    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(
            self.dialog,
            Some(Dialog::Menu { .. } | Dialog::ExplorerContext { .. })
        ) {
            return self.dialog_key(key);
        }
        if self.studio_key(key)? {
            return Ok(());
        }
        if self.dialog.is_some() {
            return self.dialog_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.doc().preview && !self.explorer_focus {
            let blocked = if ctrl {
                matches!(key.code, KeyCode::Char('a' | 'x' | 'v' | 'z' | 'y' | 'r'))
            } else {
                matches!(
                    key.code,
                    KeyCode::Char(_)
                        | KeyCode::Enter
                        | KeyCode::Backspace
                        | KeyCode::Delete
                        | KeyCode::Tab
                )
            };
            if blocked {
                self.message("preview_read_only");
                return Ok(());
            }
            if matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Left
                    | KeyCode::Right
            ) {
                let scroll = self.doc().preview_scroll;
                self.doc_mut().preview_scroll = match key.code {
                    KeyCode::Up => scroll.saturating_sub(1),
                    KeyCode::Down => scroll.saturating_add(1),
                    KeyCode::PageUp => scroll.saturating_sub(15),
                    KeyCode::PageDown => scroll.saturating_add(15),
                    KeyCode::Home => 0,
                    KeyCode::End => usize::MAX,
                    _ => scroll,
                };
                return Ok(());
            }
        }
        let navigation = matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        if navigation && !self.explorer_focus {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                let cursor = self.doc().cursor;
                self.doc_mut().anchor.get_or_insert(cursor);
            } else {
                self.doc_mut().anchor = None;
            }
        }
        if ctrl {
            match key.code {
                KeyCode::Char('q') => {
                    if self.documents.iter().any(Document::dirty) {
                        self.dialog = Some(Dialog::Confirm(Confirmation::Quit));
                    } else {
                        self.quit = true;
                    }
                }
                KeyCode::Char('s' | 'S') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.prompt(Prompt::SaveAs)
                }
                KeyCode::Char('s') => self.save_current()?,
                KeyCode::Char('o') => self.prompt(Prompt::Open),
                KeyCode::Char('n') => {
                    self.documents.push(Document::new());
                    self.active = self.documents.len() - 1;
                    self.explorer_focus = false;
                }
                KeyCode::Char('w') => {
                    if self.doc().dirty() {
                        self.dialog = Some(Dialog::Confirm(Confirmation::Close));
                    } else {
                        self.close();
                    }
                }
                KeyCode::Char('e') => self.explorer_focus = !self.explorer_focus,
                KeyCode::Char('t') => self.prompt(Prompt::NewFile),
                KeyCode::Char('d') => self.prompt(Prompt::NewDir),
                KeyCode::Char('f') => {
                    self.doc_mut().preview = false;
                    self.prompt(Prompt::Search);
                }
                KeyCode::Char('a') => {
                    self.doc_mut().anchor = Some(0);
                    self.doc_mut().cursor = self.doc().text.len_chars();
                }
                KeyCode::Char('c') => self.copy(false),
                KeyCode::Char('x') => self.copy(true),
                KeyCode::Char('v') => {
                    if self.system_clipboard.is_none() {
                        self.system_clipboard = arboard::Clipboard::new().ok();
                    }
                    let text = self
                        .system_clipboard
                        .as_mut()
                        .and_then(|c| c.get_text().ok())
                        .unwrap_or_else(|| self.clipboard.clone());
                    self.paste(&text);
                }
                KeyCode::Char('z') => self.doc_mut().undo(),
                KeyCode::Char('y') => self.doc_mut().redo(),
                KeyCode::Char('r') => self.format()?,
                KeyCode::Char('b') => self.start_build()?,
                KeyCode::Tab => self.active = (self.active + 1) % self.documents.len(),
                KeyCode::Home => self.doc_mut().cursor = 0,
                KeyCode::End => self.doc_mut().cursor = self.doc().text.len_chars(),
                _ => {}
            }
            return Ok(());
        }
        match key.code {
            KeyCode::F(1) => self.dialog = Some(Dialog::Help { scroll: 0 }),
            KeyCode::F(3) => self.find(),
            KeyCode::F(4) => self.prompt(Prompt::SaveAs),
            KeyCode::F(5) => {
                let (plugins, errors) = PluginManager::load(&self.plugin_dirs);
                self.plugins = plugins;
                self.refresh()?;
                if errors.is_empty() {
                    self.message("plugins_loaded");
                } else {
                    bail!("{}", errors.join("; "));
                }
            }
            KeyCode::F(6) => self.active = (self.active + 1) % self.documents.len(),
            KeyCode::F(7) => {
                if self.doc().preview || self.doc().can_preview() {
                    self.doc_mut().preview = !self.doc().preview;
                    self.message(if self.doc().preview {
                        "preview_read_only"
                    } else {
                        "source_mode"
                    });
                } else {
                    self.message("preview_md_only");
                }
            }
            KeyCode::F(8) => self.dialog = Some(Dialog::Output { scroll: 0 }),
            KeyCode::F(9) => self.open_diagram(),
            _ if self.explorer_focus => self.explorer_key(key)?,
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                self.doc_mut().insert(&c.to_string())
            }
            KeyCode::Enter => self.doc_mut().newline(),
            KeyCode::Tab => {
                let spaces = " ".repeat(self.studio.config.indent.clamp(1, 16));
                self.doc_mut().insert(&spaces);
            }
            KeyCode::Backspace => self.doc_mut().backspace(),
            KeyCode::Delete => self.doc_mut().delete(),
            KeyCode::Left => self.doc_mut().left(),
            KeyCode::Right => self.doc_mut().right(),
            KeyCode::Up => self.doc_mut().move_vertical(-1),
            KeyCode::Down => self.doc_mut().move_vertical(1),
            KeyCode::Home => self.doc_mut().home(),
            KeyCode::End => self.doc_mut().end(),
            KeyCode::PageUp => self.doc_mut().move_vertical(-20),
            KeyCode::PageDown => self.doc_mut().move_vertical(20),
            _ => {}
        }
        Ok(())
    }
    fn explorer_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::F(2) => {
                if self.entries.get(self.selected).is_some() {
                    self.prompt(Prompt::RenameFile);
                }
            }
            KeyCode::Delete => {
                if let Some(entry) = self.entries.get(self.selected) {
                    self.dialog = Some(Dialog::Confirm(Confirmation::Delete(entry.path.clone())));
                }
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(entry) = self.entries.get(self.selected) {
                    let path = entry.path.clone();
                    if entry.directory {
                        self.directory = path;
                        self.selected = 0;
                        self.explorer_offset = 0;
                        self.refresh()?;
                    } else {
                        self.open(&path)?;
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(parent) = self.directory.parent() {
                    self.directory = parent.to_owned();
                    self.selected = 0;
                    self.explorer_offset = 0;
                    self.refresh()?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn explorer_context_action(&mut self, action: usize, path: PathBuf) -> Result<()> {
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.path == path)
            .unwrap_or(self.selected);
        self.explorer_focus = true;
        match action {
            0 => {
                if path.is_dir() {
                    self.directory = path;
                    self.selected = 0;
                    self.explorer_offset = 0;
                    self.refresh()?;
                } else {
                    self.open(&path)?;
                }
            }
            1 => self.prompt(Prompt::RenameFile),
            2 => self.dialog = Some(Dialog::Confirm(Confirmation::Delete(path))),
            3 => self.copy_explorer_path(&path, false)?,
            4 => self.copy_explorer_path(&path, true)?,
            _ => {}
        }
        Ok(())
    }
    fn copy_explorer_path(&mut self, path: &Path, relative: bool) -> Result<()> {
        let value = if relative {
            relative_path(&self.root, path)
                .context("Cannot make this path relative to the project")?
        } else {
            path.to_owned()
        };
        let text = value.to_string_lossy().into_owned();
        self.clipboard = text.clone();
        if self.system_clipboard.is_none() {
            self.system_clipboard = arboard::Clipboard::new().ok();
        }
        let clipboard = self
            .system_clipboard
            .as_mut()
            .context("System clipboard is unavailable")?;
        clipboard.set_text(text)?;
        self.message("path_copied");
        Ok(())
    }
    fn dialog_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(self.dialog, Some(Dialog::Diagram)) {
            return self.diagram_key(key);
        }
        let dialog = self.dialog.take().unwrap();
        if key.code == KeyCode::Esc {
            return Ok(());
        }
        match dialog {
            Dialog::Diagram | Dialog::Workbench => unreachable!(),
            Dialog::Menu {
                mut menu,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        selected = (selected + 1).min(MENU_ITEMS[menu].len().saturating_sub(1))
                    }
                    KeyCode::Left => {
                        menu = (menu + MENU_NAMES.len() - 1) % MENU_NAMES.len();
                        selected = 0;
                    }
                    KeyCode::Right => {
                        menu = (menu + 1) % MENU_NAMES.len();
                        selected = 0;
                    }
                    KeyCode::Enter => {
                        let action = MENU_ITEMS[menu].get(selected).map(|item| item.2);
                        if let Some(action) = action {
                            self.activate_menu(action)?;
                        }
                        return Ok(());
                    }
                    _ => {}
                }
                self.dialog = Some(Dialog::Menu { menu, selected });
            }
            Dialog::ExplorerContext {
                path,
                mut selected,
                x,
                y,
            } => match key.code {
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    self.dialog = Some(Dialog::ExplorerContext {
                        path,
                        selected,
                        x,
                        y,
                    });
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(EXPLORER_CONTEXT_ITEMS.len() - 1);
                    self.dialog = Some(Dialog::ExplorerContext {
                        path,
                        selected,
                        x,
                        y,
                    });
                }
                KeyCode::Enter => self.explorer_context_action(selected, path)?,
                _ => {
                    self.dialog = Some(Dialog::ExplorerContext {
                        path,
                        selected,
                        x,
                        y,
                    });
                }
            },
            Dialog::Prompt { kind, mut input } => {
                match key.code {
                    KeyCode::Enter => {
                        if input.trim().is_empty() {
                            self.dialog = Some(Dialog::Prompt { kind, input });
                            return Ok(());
                        }
                        if let Err(error) = self.submit(kind, &input) {
                            self.dialog = Some(Dialog::Prompt { kind, input });
                            return Err(error);
                        }
                        return Ok(());
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input.clear()
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        input.push(c)
                    }
                    _ => {}
                }
                self.dialog = Some(Dialog::Prompt { kind, input });
            }
            Dialog::Confirm(action) => {
                if key.code == KeyCode::Enter {
                    match action {
                        Confirmation::Quit => self.quit = true,
                        Confirmation::Close => self.close(),
                        Confirmation::Overwrite(path) => self.save(&path, true)?,
                        Confirmation::Delete(path) => self.delete_path(&path)?,
                    }
                } else {
                    self.dialog = Some(Dialog::Confirm(action));
                }
            }
            Dialog::Plugins { mut selected } => {
                let commands = self.plugins.commands();
                match key.code {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        selected = (selected + 1).min(commands.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        if let Some((index, event, _)) = commands.get(selected) {
                            self.execute_plugin(*index, event)?;
                        }
                        return Ok(());
                    }
                    _ => {}
                }
                self.dialog = Some(Dialog::Plugins { selected });
            }
            Dialog::Help { mut scroll } => {
                scroll = self.scroll_key(key, scroll, self.i18n.t("help_text").lines().count());
                self.dialog = Some(Dialog::Help { scroll });
            }
            Dialog::Language { mut selected } => {
                match key.code {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(Language::ALL.len() - 1),
                    KeyCode::Home => selected = 0,
                    KeyCode::End => selected = Language::ALL.len() - 1,
                    KeyCode::Enter => {
                        if let Err(error) = self.set_language(Language::ALL[selected]) {
                            self.dialog = Some(Dialog::Language { selected });
                            return Err(error);
                        }
                        return Ok(());
                    }
                    _ => {}
                }
                self.dialog = Some(Dialog::Language { selected });
            }
            Dialog::FolderPicker {
                directory,
                entries,
                mut selected,
                mut offset,
            } => {
                let parent_offset = usize::from(directory.parent().is_some());
                let last = entries
                    .len()
                    .saturating_add(parent_offset)
                    .saturating_sub(1);
                match key.code {
                    KeyCode::Char('o' | 'O') => {
                        self.dialog = Some(Dialog::FolderPicker {
                            directory,
                            entries,
                            selected,
                            offset,
                        });
                        self.folder_picker_choose()?;
                        return Ok(());
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.dialog = Some(Dialog::FolderPicker {
                            directory,
                            entries,
                            selected,
                            offset,
                        });
                        self.folder_picker_choose()?;
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        self.dialog = Some(Dialog::FolderPicker {
                            directory,
                            entries,
                            selected,
                            offset,
                        });
                        self.folder_picker_enter()?;
                        return Ok(());
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(last),
                    KeyCode::PageUp => selected = selected.saturating_sub(10),
                    KeyCode::PageDown => selected = (selected + 10).min(last),
                    KeyCode::Home => selected = 0,
                    KeyCode::End => selected = last,
                    KeyCode::Backspace => {
                        if let Some(parent) = directory.parent() {
                            let parent = parent.to_owned();
                            let next_entries = Self::folder_entries(&parent)?;
                            self.dialog = Some(Dialog::FolderPicker {
                                directory: parent,
                                entries: next_entries,
                                selected: 0,
                                offset: 0,
                            });
                            return Ok(());
                        }
                    }
                    _ => {}
                }
                offset = offset.min(last);
                self.dialog = Some(Dialog::FolderPicker {
                    directory,
                    entries,
                    selected,
                    offset,
                });
            }
            Dialog::Output { mut scroll } => {
                scroll = self.scroll_key(key, scroll, self.build_output.lines().count());
                self.dialog = Some(Dialog::Output { scroll });
            }
        }
        Ok(())
    }
    fn activate_menu(&mut self, action: &str) -> Result<()> {
        self.dialog = None;
        match action {
            "open_folder" => self.open_folder_picker()?,
            "recent_projects" => self.command("projects")?,
            "new_dir" => self.prompt(Prompt::NewDir),
            "new" => {
                self.documents.push(Document::new());
                self.active = self.documents.len() - 1;
            }
            "save" => self.save_current()?,
            "save_as" => self.prompt(Prompt::SaveAs),
            "close" => {
                if self.doc().dirty() {
                    self.dialog = Some(Dialog::Confirm(Confirmation::Close));
                } else {
                    self.close();
                }
            }
            "quit" => {
                if self.documents.iter().any(Document::dirty) {
                    self.dialog = Some(Dialog::Confirm(Confirmation::Quit));
                } else {
                    self.quit = true;
                }
            }
            "undo" => self.doc_mut().undo(),
            "redo" => self.doc_mut().redo(),
            "cut" => self.copy(true),
            "copy" => self.copy(false),
            "paste" => {
                if self.system_clipboard.is_none() {
                    self.system_clipboard = arboard::Clipboard::new().ok();
                }
                let text = self
                    .system_clipboard
                    .as_mut()
                    .and_then(|c| c.get_text().ok())
                    .unwrap_or_else(|| self.clipboard.clone());
                self.paste(&text);
            }
            "find" => self.prompt(Prompt::Search),
            "format" => self.format()?,
            "select_all" => {
                self.doc_mut().anchor = Some(0);
                self.doc_mut().cursor = self.doc().text.len_chars();
            }
            "explorer" => self.explorer_focus = !self.explorer_focus,
            "preview" => {
                if self.doc().preview || self.doc().can_preview() {
                    self.doc_mut().preview = !self.doc().preview;
                }
            }
            "output" => self.dialog = Some(Dialog::Output { scroll: 0 }),
            "next_match" => self.find(),
            "next_tab" => self.active = (self.active + 1) % self.documents.len(),
            "previous_tab" => {
                self.active = (self.active + self.documents.len() - 1) % self.documents.len()
            }
            "build" => self.start_build()?,
            "run" => self.command("cargo.run")?,
            "palette" => self.command("palette")?,
            "help" => self.dialog = Some(Dialog::Help { scroll: 0 }),
            "language" => {
                let selected = Language::ALL
                    .iter()
                    .position(|l| *l == self.i18n.language)
                    .unwrap_or(0);
                self.dialog = Some(Dialog::Language { selected });
            }
            "terminal" => self.command("terminal.toggle")?,
            _ => {}
        }
        Ok(())
    }
    pub(crate) fn open_diagram(&mut self) {
        let blocks = diagram::blocks(self.doc());
        if blocks.is_empty() {
            self.message("no_diagrams");
            return;
        }
        let cursor = self.doc().text.char_to_byte(self.doc().cursor);
        let selected = blocks
            .iter()
            .position(|b| cursor <= b.end)
            .unwrap_or(blocks.len() - 1);
        let mut view = DiagramView::new(blocks, selected);
        view.start();
        self.diagram = Some(view);
        self.dialog = Some(Dialog::Diagram);
    }
    fn diagram_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Esc | KeyCode::F(9)) {
            self.dialog = None;
            self.diagram = None;
            return Ok(());
        }
        if key.code == KeyCode::Char('e') {
            let start = self
                .diagram
                .as_ref()
                .map(|d| d.blocks[d.selected].start)
                .unwrap_or(0);
            self.doc_mut().cursor = self
                .doc()
                .text
                .byte_to_char(start.min(self.doc().text.len_bytes()));
            self.doc_mut().preview = false;
            self.explorer_focus = false;
            self.dialog = None;
            self.diagram = None;
            return Ok(());
        }
        if key.code == KeyCode::Char('/') {
            return self.command("diagram.labels");
        }
        if key.code == KeyCode::Char('s') {
            return self.command("diagram.svg");
        }
        if key.code == KeyCode::Char('p') {
            return self.command("diagram.png");
        }
        if let Some(view) = self.diagram.as_mut() {
            match key.code {
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    view.zoom = view.zoom.saturating_add(25).min(800)
                }
                KeyCode::Char('-') => {
                    view.zoom =
                        view.zoom
                            .saturating_sub(25)
                            .max(if view.graphics { 25 } else { 100 })
                }
                KeyCode::Char('g') => {
                    view.graphics = !view.graphics;
                    view.zoom = 100;
                    view.pan_x = 0;
                    view.pan_y = 0;
                }
                KeyCode::Char('0') | KeyCode::Home => {
                    view.zoom = 100;
                    view.pan_x = 0;
                    view.pan_y = 0;
                }
                KeyCode::Left => view.pan_x = view.pan_x.saturating_sub(40),
                KeyCode::Right => view.pan_x = view.pan_x.saturating_add(40),
                KeyCode::Up => view.pan_y = view.pan_y.saturating_sub(40),
                KeyCode::Down => view.pan_y = view.pan_y.saturating_add(40),
                KeyCode::PageUp => view.pan_y = view.pan_y.saturating_sub(200),
                KeyCode::PageDown => view.pan_y = view.pan_y.saturating_add(200),
                KeyCode::Char(']') | KeyCode::Tab => {
                    view.selected = (view.selected + 1) % view.blocks.len();
                    view.start();
                }
                KeyCode::Char('[') | KeyCode::BackTab => {
                    view.selected = (view.selected + view.blocks.len() - 1) % view.blocks.len();
                    view.start();
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub fn poll_diagram(&mut self) {
        if let Some(view) = self.diagram.as_mut() {
            view.poll();
        }
    }
    fn scroll_key(&self, key: KeyEvent, scroll: u16, count: usize) -> u16 {
        let max = count.saturating_sub(1).min(u16::MAX as usize) as u16;
        match key.code {
            KeyCode::Down => scroll.saturating_add(1).min(max),
            KeyCode::PageDown => scroll.saturating_add(10).min(max),
            KeyCode::Up => scroll.saturating_sub(1),
            KeyCode::PageUp => scroll.saturating_sub(10),
            KeyCode::Home => 0,
            KeyCode::End => max,
            _ => scroll,
        }
    }
    fn set_language(&mut self, language: Language) -> Result<()> {
        fs::create_dir_all(self.root.join(".reditor"))?;
        let mut config = self.studio.config.clone();
        config.language = language;
        crate::studio::save_config(&self.root, &config)?;
        self.studio.config = config;
        self.i18n = I18n::new(language);
        self.message("config_saved");
        Ok(())
    }
    fn submit(&mut self, kind: Prompt, input: &str) -> Result<()> {
        let path = self.resolve(input);
        match kind {
            Prompt::Open => {
                if path.is_dir() {
                    self.switch_project(&path)?;
                } else {
                    self.open(&path)?;
                }
            }
            Prompt::SaveAs => {
                if path.exists() && self.doc().path.as_ref() != path.canonicalize().ok().as_ref() {
                    if path.is_dir() {
                        bail!("Path is a directory");
                    }
                    self.dialog = Some(Dialog::Confirm(Confirmation::Overwrite(path)));
                } else {
                    self.save(&path, false)?;
                }
            }
            Prompt::NewFile => {
                // create_new guarantees that an existing file is never truncated.
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                self.refresh()?;
                self.open(&path)?;
                self.message("created");
            }
            Prompt::NewDir => {
                fs::create_dir_all(&path)?;
                self.refresh()?;
                self.message("created");
            }
            Prompt::Search => {
                self.search = input.to_owned();
                self.find();
            }
            Prompt::RenameFile => self.rename_selected(input)?,
        }
        Ok(())
    }
    fn find(&mut self) {
        self.doc_mut().preview = false;
        let needle = self.search.clone();
        if !self.doc_mut().find_next(&needle) {
            self.message("not_found");
        } else {
            self.message("ready");
            self.explorer_focus = false;
        }
    }
    fn copy(&mut self, cut: bool) {
        if let Some(text) = self.doc().selected_text() {
            self.clipboard = text.clone();
            if self.system_clipboard.is_none() {
                self.system_clipboard = arboard::Clipboard::new().ok();
            }
            if let Some(clipboard) = self.system_clipboard.as_mut() {
                let _ = clipboard.set_text(text);
            }
            if cut {
                self.doc_mut().delete();
            }
        }
    }
    pub(crate) fn execute_plugin(&mut self, index: usize, event: &str) -> Result<()> {
        let plugin = self
            .plugins
            .plugins
            .get(index)
            .context("Plugin not found")?;
        let settings = crate::settings::plugin_settings(&self.root, &plugin.manifest.name)?;
        let result = self.plugins.execute_with_settings(
            index,
            &PluginInput {
                event,
                text: &self.doc().text.to_string(),
                path: self
                    .doc()
                    .path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                language: &self.i18n.code().to_lowercase(),
            },
            &settings,
        )?;
        if let Some(action) = result.action {
            return self.plugin_action(index, action);
        }
        if let Some(text) = result.text {
            self.doc_mut().replace(&text);
        }
        self.message("plugin_ok");
        if let Some(message) = result.message {
            self.status = message;
        }
        if let Some(panel) = result.panel {
            self.studio.plugin_panel = Some(panel);
            self.studio.bottom_visible = true;
            self.studio.bottom_plugin = true;
            self.studio.terminal_focus = false;
        }
        Ok(())
    }
    pub(crate) fn format(&mut self) -> Result<()> {
        if self
            .doc()
            .path
            .as_ref()
            .is_some_and(|p| !p.extension().is_some_and(|s| s.eq_ignore_ascii_case("rs")))
        {
            bail!("{}", self.i18n.t("format_rust_only"));
        }
        let mut command = Command::new("rustfmt");
        command.args(["--edition", "2024", "--emit", "stdout"]);
        let (success, stdout, stderr) =
            process::run(command, self.doc().text.to_string(), Duration::from_secs(5))?;
        if !success {
            bail!("rustfmt: {}", stderr.trim());
        }
        self.doc_mut().replace(&stdout);
        self.message("format_ok");
        Ok(())
    }
    fn start_build(&mut self) -> Result<()> {
        if self.build.is_some() {
            self.message("running");
            return Ok(());
        }
        let start = self
            .doc()
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .unwrap_or(&self.root);
        let root = start
            .ancestors()
            .find(|p| p.join("Cargo.toml").is_file())
            .context("Cargo.toml not found")?
            .to_owned();
        let (sender, receiver) = mpsc::channel();
        self.build_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.build_cancel.clone();
        self.build_worker = Some(thread::spawn(move || {
            let mut command = Command::new("cargo");
            command
                .args(["check", "--color", "never"])
                .current_dir(root);
            let result =
                process::run_cancellable(command, String::new(), Duration::from_secs(120), cancel);
            let _ = sender.send(result);
        }));
        self.build = Some(receiver);
        self.message("running");
        Ok(())
    }
    pub fn poll_build(&mut self) {
        if let Some(result) = self.build.as_ref().and_then(|r| r.try_recv().ok()) {
            self.build = None;
            if let Some(worker) = self.build_worker.take() {
                let _ = worker.join();
            }
            match result {
                Ok((success, stdout, stderr)) => {
                    self.build_output = format!("{stdout}{stderr}");
                    self.message(if success { "build_ok" } else { "build_failed" });
                    self.status_error = !success;
                }
                Err(error) => {
                    self.build_output = format!("{error:#}");
                    self.error(error);
                }
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.build_cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = self.build_worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    #[test]
    fn switching_projects_restores_unsaved_buffers() -> Result<()> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let file = first.path().join("notes.txt");
        fs::write(&file, "saved")?;
        let mut app = App::new(first.path().into(), Language::Ru, vec![])?;
        app.open(&file)?;
        app.doc_mut().replace("unsaved");
        app.switch_project(second.path())?;
        assert_eq!(app.root, second.path().canonicalize()?);
        app.switch_project(first.path())?;
        assert_eq!(app.doc().text.to_string(), "unsaved");
        assert_eq!(fs::read_to_string(file)?, "saved");
        Ok(())
    }
    #[test]
    fn unsaved_tabs_require_confirmation() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut app = App::new(root.path().to_owned(), Language::Ru, vec![])?;
        app.paste("fn main() {}");
        app.key(ctrl('n'));
        app.key(ctrl('q'));
        assert!(!app.quit);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Confirm(Confirmation::Quit))
        ));
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.quit);
        app.key(ctrl('q'));
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.quit);
        Ok(())
    }
    #[test]
    fn file_directory_workflow() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut app = App::new(root.path().to_owned(), Language::En, vec![])?;
        app.submit(Prompt::NewDir, "src/nested")?;
        app.submit(Prompt::NewFile, "src/nested/main.rs")?;
        app.paste("fn main() {}\n");
        app.key(ctrl('s'));
        assert!(!app.status_error, "{}", app.status);
        assert_eq!(
            fs::read_to_string(root.path().join("src/nested/main.rs"))?,
            "fn main() {}\n"
        );
        assert!(app.submit(Prompt::NewFile, "src/nested/main.rs").is_err());
        app.submit(Prompt::SaveAs, "notes.txt")?;
        assert!(root.path().join("notes.txt").exists());
        Ok(())
    }
    #[test]
    fn explorer_rename_move_and_confirmed_delete_update_open_paths() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source_dir = root.path().join("source");
        let target_dir = root.path().join("target");
        fs::create_dir(&source_dir)?;
        fs::create_dir(&target_dir)?;
        let file = source_dir.join("note.txt");
        fs::write(&file, "text")?;
        let mut app = App::new(root.path().to_owned(), Language::En, vec![])?;
        app.open(&file)?;
        app.directory = source_dir.clone();
        app.refresh()?;
        app.selected = app
            .entries
            .iter()
            .position(|entry| entry.path == file)
            .unwrap();
        app.submit(Prompt::RenameFile, "renamed.txt")?;
        let renamed = source_dir.join("renamed.txt");
        assert_eq!(
            app.doc().path.as_deref(),
            Some(renamed.canonicalize()?.as_path())
        );
        app.move_entry(&renamed, &target_dir)?;
        let moved = target_dir.join("renamed.txt");
        assert_eq!(
            app.doc().path.as_deref(),
            Some(moved.canonicalize()?.as_path())
        );
        app.directory = target_dir;
        app.refresh()?;
        app.selected = app
            .entries
            .iter()
            .position(|entry| entry.path == moved)
            .unwrap();
        app.explorer_focus = true;
        app.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(matches!(
            app.dialog,
            Some(Dialog::Confirm(Confirmation::Delete(_)))
        ));
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!moved.exists());
        assert!(app.doc().path.is_none());
        Ok(())
    }
    #[test]
    fn explorer_paths_can_be_copied_project_relative() {
        let project = Path::new("/workspace/demo");
        assert_eq!(
            relative_path(project, Path::new("/workspace/demo/src/main.rs")),
            Some(PathBuf::from("src/main.rs"))
        );
        assert_eq!(
            relative_path(project, Path::new("/workspace/notes.txt")),
            Some(PathBuf::from("../notes.txt"))
        );
        assert_eq!(relative_path(project, project), Some(PathBuf::from(".")));
    }
    #[test]
    fn explorer_renames_and_deletes_nonempty_directories() -> Result<()> {
        let root = tempfile::tempdir()?;
        let folder = root.path().canonicalize()?.join("old-folder");
        fs::create_dir(&folder)?;
        fs::write(folder.join("inside.txt"), "keep")?;
        let mut app = App::new(root.path().to_owned(), Language::En, vec![])?;
        app.selected = app
            .entries
            .iter()
            .position(|entry| entry.path == folder)
            .unwrap();
        app.submit(Prompt::RenameFile, "new-folder")?;
        let renamed = app.root.join("new-folder");
        assert!(renamed.join("inside.txt").is_file());
        app.selected = app
            .entries
            .iter()
            .position(|entry| entry.path == renamed)
            .unwrap();
        app.explorer_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;
        app.dialog_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(!renamed.exists());
        Ok(())
    }
    #[test]
    fn failed_save_hook_preserves_disk_and_buffer() -> Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("plugin");
        fs::create_dir(&dir)?;
        fs::write(
            dir.join("plugin.toml"),
            "name='broken'\nentry='main.lua'\nhooks=['before_save']",
        )?;
        fs::write(
            dir.join("main.lua"),
            "return function(ctx) error('hook failed') end",
        )?;
        let path = root.path().join("main.rs");
        fs::write(&path, "original")?;
        let mut app = App::new(root.path().to_owned(), Language::En, vec![dir])?;
        app.open(&path)?;
        app.doc_mut().replace("edited");
        assert!(app.save(&path, false).is_err());
        assert_eq!(fs::read_to_string(&path)?, "original");
        assert_eq!(app.doc().text.to_string(), "edited");
        assert!(app.doc().dirty());
        Ok(())
    }
}
