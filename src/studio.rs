use crate::{
    app::{App, Config, Dialog},
    document::Document,
    i18n::Language,
    lsp,
    plugins::PluginPanel,
    session,
    terminal::TerminalSession,
    workspace,
};
use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub enum Action {
    Command(String),
    Jump(PathBuf, usize, usize, bool),
    Plugin(usize, String),
    PluginMenu(usize),
    Project(PathBuf),
    Complete(Value),
    Fix(Value),
    Package(String),
    Target(String),
    GitDiff(PathBuf),
    Stage(PathBuf),
    Diagram(usize),
    RemoteBrowse(String, String),
    RemoteOpen(String, String),
}
#[derive(Clone)]
pub struct Choice {
    pub label: String,
    pub action: Action,
}
#[derive(Clone)]
pub enum PanelMode {
    Select,
    Input(String),
    Text,
    Review,
}
pub struct Panel {
    pub title: String,
    pub input: String,
    pub body: String,
    pub items: Vec<Choice>,
    pub selected: usize,
    pub scroll: usize,
    pub mode: PanelMode,
}
impl Panel {
    pub fn filtered(&self) -> Vec<usize> {
        let query = self.input.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let label = item.label.to_lowercase();
                let mut chars = label.chars();
                query
                    .chars()
                    .all(|needle| chars.by_ref().any(|ch| ch == needle))
            })
            .map(|(i, _)| i)
            .collect()
    }
}
pub struct Change {
    pub path: Option<PathBuf>,
    pub id: Option<u64>,
    pub before: String,
    pub after: String,
}
pub(crate) enum JobResult {
    Remote(Box<crate::remote::Outcome>),
    Proofread {
        id: u64,
        revision: u64,
        report: crate::proofreader::CheckReport,
        show_panel: bool,
    },
    Formatted {
        id: u64,
        revision: u64,
        before: String,
        after: String,
        check: bool,
    },
    Files(Vec<PathBuf>),
    Hits(Vec<workspace::Hit>),
    Metadata(Value),
    Git(String, String),
    Text(String),
    Export(PathBuf),
    Changes(Vec<Change>),
}
struct Job {
    receive: Receiver<Result<JobResult>>,
    kind: String,
    display: bool,
}
pub struct Studio {
    pub config: Config,
    pub remote: crate::remote::State,
    pub activity_plugin: Option<usize>,
    pub activity_scroll: usize,
    pub(crate) proofread: Option<crate::proofreader::ProofreadState>,
    pub(crate) proofread_generation: u64,
    pub(crate) proofread_observed: Option<(u64, u64)>,
    pub(crate) proofread_requested: Option<(u64, u64)>,
    pub(crate) proofread_changed: Instant,
    pub(crate) proofread_job: Option<Receiver<crate::proofreader::ProofreadResult>>,
    pub(crate) pending_uploads: std::collections::VecDeque<(PathBuf, String)>,
    pub panel: Option<Panel>,
    pub terminals: Vec<TerminalSession>,
    pub terminal_active: usize,
    pub bottom_visible: bool,
    pub terminal_focus: bool,
    pub bottom_height: u16,
    pub explorer_width: Option<u16>,
    pub split_percent: u16,
    pub bottom_plugin: bool,
    pub plugin_panel: Option<PluginPanel>,
    pub split: Option<usize>,
    pub split_preview: bool,
    pub split_mermaid: bool,
    pub split_right_focus: bool,
    pub wrap_width: usize,
    pub lsp: Option<lsp::Client>,
    pub lsp_error: Option<String>,
    lsp_attempted: bool,
    pub branch: String,
    pub git_lines: HashMap<PathBuf, HashMap<usize, char>>,
    pub package: Option<String>,
    pub target: Option<String>,
    pub release: bool,
    pub run_args: String,
    pub changes: Vec<Change>,
    pub needle: String,
    pub project_replace: bool,
    job: Option<Job>,
    last_session: Instant,
    last_lsp: Instant,
    last_diagram: Instant,
    diagram_revision: u64,
    completion_origin: Option<(u64, u64, usize)>,
    lsp_origin: Option<(u64, u64)>,
    git_job: Option<Receiver<Result<(String, workspace::GitLines)>>>,
    last_git: Instant,
}
impl Studio {
    pub fn new(root: &Path, language: Language) -> Result<Self> {
        let mut config = crate::settings::load(root)?;
        config.language = language;
        Ok(Self {
            config,
            remote: crate::remote::State::load(root)?,
            activity_plugin: None,
            activity_scroll: 0,
            proofread: None,
            proofread_generation: 0,
            proofread_observed: None,
            proofread_requested: None,
            proofread_changed: Instant::now(),
            proofread_job: None,
            pending_uploads: Default::default(),
            panel: None,
            terminals: vec![],
            terminal_active: 0,
            bottom_visible: false,
            terminal_focus: false,
            bottom_height: 12,
            explorer_width: None,
            split_percent: 50,
            bottom_plugin: false,
            plugin_panel: None,
            split: None,
            split_preview: false,
            split_mermaid: false,
            split_right_focus: false,
            wrap_width: 80,
            lsp: None,
            lsp_error: None,
            lsp_attempted: false,
            branch: String::new(),
            git_lines: HashMap::new(),
            package: None,
            target: None,
            release: false,
            run_args: String::new(),
            changes: vec![],
            needle: String::new(),
            project_replace: false,
            job: None,
            last_session: Instant::now(),
            last_lsp: Instant::now(),
            last_diagram: Instant::now(),
            diagram_revision: 0,
            completion_origin: None,
            lsp_origin: None,
            git_job: None,
            last_git: Instant::now() - Duration::from_secs(4),
        })
    }
    pub fn terminal_mut(&mut self) -> Option<&mut TerminalSession> {
        self.terminals.get_mut(self.terminal_active)
    }
}
pub fn save_config(root: &Path, config: &Config) -> Result<()> {
    fs::create_dir_all(root.join(".reditor"))?;
    let mut temp = tempfile::NamedTempFile::new_in(root.join(".reditor"))?;
    temp.write_all(crate::settings::project_config(config)?.as_bytes())?;
    temp.persist(root.join(".reditor/config.toml"))?;
    Ok(())
}
pub fn shortcut(key: KeyEvent) -> String {
    let mut name = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        name.push_str("Ctrl+");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        name.push_str("Alt+");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        name.push_str("Shift+");
    }
    name.push_str(&match key.code {
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Tab => "Tab".into(),
        _ => format!("{:?}", key.code),
    });
    name
}
fn commands() -> &'static [(&'static str, &'static str)] {
    &[
        ("file.save_all", "save_all"),
        ("plugins.sidebar", "plugin_sidebar"),
        ("projects", "recent_projects"),
        ("project.open", "open_project"),
        ("files", "quick_open"),
        ("search.project", "project_search"),
        ("replace.file", "replace_file"),
        ("replace.project", "replace_project"),
        ("terminal.toggle", "terminal"),
        ("terminal.new", "terminal_new"),
        ("terminal.stop", "stop"),
        ("terminal.close", "terminal_close"),
        ("terminal.grow", "panel_grow"),
        ("terminal.shrink", "panel_shrink"),
        ("layout.explorer.grow", "explorer_grow"),
        ("layout.explorer.shrink", "explorer_shrink"),
        ("layout.split.grow", "split_grow"),
        ("layout.split.shrink", "split_shrink"),
        ("layout.reset", "layout_reset"),
        ("cargo.check", "cargo_check"),
        ("cargo.build", "cargo_build"),
        ("cargo.run", "cargo_run"),
        ("cargo.test", "cargo_test"),
        ("cargo.clippy", "cargo_clippy"),
        ("cargo.package", "cargo_package"),
        ("cargo.target", "cargo_target"),
        ("cargo.profile", "cargo_profile"),
        ("cargo.args", "run_args"),
        ("cargo.locations", "tool_locations"),
        ("lsp.complete", "completion"),
        ("lsp.hover", "hover"),
        ("lsp.definition", "definition"),
        ("lsp.references", "references"),
        ("lsp.rename", "rename"),
        ("lsp.actions", "code_actions"),
        ("lsp.diagnostics", "diagnostics"),
        ("lsp.restart", "lsp_restart"),
        ("split.next", "split_window"),
        ("split.preview", "split_preview"),
        ("split.diagram", "split_diagram"),
        ("split.close", "split_close"),
        ("git.status", "git_status"),
        ("git.diff", "git_diff"),
        ("git.stage", "git_stage"),
        ("git.commit", "git_commit"),
        ("settings", "settings"),
        ("settings.user", "settings_user"),
        ("settings.project", "settings_project"),
        ("settings.theme", "theme"),
        ("settings.indent", "indent"),
        ("settings.wrap", "wrap"),
        ("plugins.settings", "plugin_settings"),
        ("diagram.list", "diagram_list"),
        ("diagram.labels", "diagram_labels"),
        ("diagram.connections", "diagram_connections"),
        ("diagram.svg", "export_svg"),
        ("diagram.png", "export_png"),
        ("format", "format"),
    ]
}
impl App {
    fn studio_panel(&mut self, title: String, mode: PanelMode, body: String, items: Vec<Choice>) {
        self.studio.panel = Some(Panel {
            title,
            input: String::new(),
            body,
            items,
            selected: 0,
            scroll: 0,
            mode,
        });
        self.dialog = Some(Dialog::Workbench);
        self.studio.terminal_focus = false;
    }
    pub(crate) fn select(&mut self, key: &str, items: Vec<Choice>) {
        self.studio_panel(
            self.i18n.t(key).into(),
            PanelMode::Select,
            String::new(),
            items,
        );
    }
    pub(crate) fn input(&mut self, key: &str, kind: &str) {
        self.studio_panel(
            self.i18n.t(key).into(),
            PanelMode::Input(kind.into()),
            String::new(),
            vec![],
        );
    }
    pub(crate) fn text_panel(&mut self, key: &str, body: String) {
        self.studio_panel(self.i18n.t(key).into(), PanelMode::Text, body, vec![]);
    }
    pub(crate) fn job(
        &mut self,
        kind: &str,
        work: impl FnOnce() -> Result<JobResult> + Send + 'static,
    ) -> Result<()> {
        if self.studio.job.is_some() {
            bail!("{}", self.i18n.t("running"));
        }
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            let _ = send.send(work());
        });
        self.studio.job = Some(Job {
            receive,
            kind: kind.into(),
            display: true,
        });
        self.text_panel("running", String::new());
        Ok(())
    }
    pub(crate) fn show_plugin_commands(&mut self, index: usize) -> Result<()> {
        let plugin = self
            .plugins
            .plugins
            .get(index)
            .context("Plugin not found")?;
        let title = plugin.manifest.name.clone();
        let language = self.i18n.code().to_lowercase();
        let items = plugin
            .manifest
            .commands
            .iter()
            .map(|command| Choice {
                label: command
                    .titles
                    .get(&language)
                    .unwrap_or(&command.title)
                    .clone(),
                action: Action::Plugin(index, command.id.clone()),
            })
            .collect();
        self.studio.activity_plugin = Some(index);
        self.studio_panel(title, PanelMode::Select, String::new(), items);
        Ok(())
    }
    pub(crate) fn job_running(&self) -> bool {
        self.studio.job.is_some()
    }
    pub fn command(&mut self, command: &str) -> Result<()> {
        match command {
            "palette" => {
                let mut items: Vec<Choice> = commands()
                    .iter()
                    .map(|(id, key)| Choice {
                        label: format!("{}  ·  {id}", self.i18n.t(key)),
                        action: Action::Command((*id).into()),
                    })
                    .collect();
                items.extend(
                    self.plugins
                        .commands_for(&self.i18n.code().to_lowercase())
                        .into_iter()
                        .map(|(index, id, title)| Choice {
                            label: title,
                            action: Action::Plugin(index, id),
                        }),
                );
                self.select("palette", items);
            }
            "plugins.sidebar" => self.select(
                "plugin_sidebar",
                self.plugins
                    .plugins
                    .iter()
                    .enumerate()
                    .map(|(index, plugin)| Choice {
                        label: plugin.manifest.name.clone(),
                        action: Action::PluginMenu(index),
                    })
                    .collect(),
            ),
            "projects" => {
                let mut projects = crate::settings::recent_projects()?;
                if !projects.contains(&self.root) {
                    projects.insert(0, self.root.clone());
                }
                self.select(
                    "recent_projects",
                    projects
                        .into_iter()
                        .map(|path| Choice {
                            label: path.display().to_string(),
                            action: Action::Project(path),
                        })
                        .collect(),
                );
            }
            "project.open" => self.input("open_project", "project.open"),
            "files" => {
                let root = self.root.clone();
                self.job("files", move || {
                    Ok(JobResult::Files(workspace::files(&root)))
                })?;
            }
            "search.project" => self.input("project_search", "search.project"),
            "replace.file" | "replace.project" => {
                self.studio.project_replace = command.ends_with("project");
                self.input("replace_find", "replace.find");
            }
            "file.save_all" => {
                anyhow::ensure!(
                    !self.documents.iter().any(|d| d.dirty() && d.path.is_none()),
                    "Save untitled tabs with Ctrl+S first"
                );
                let active = self.active;
                for index in 0..self.documents.len() {
                    if self.documents[index].dirty() {
                        let path = self.documents[index]
                            .path
                            .clone()
                            .context("Save untitled tabs with Ctrl+S first")?;
                        self.active = index;
                        if let Err(e) = self.save(&path, false) {
                            self.active = active;
                            return Err(e);
                        }
                    }
                }
                self.active = active;
            }
            "terminal.toggle" => {
                if self.studio.terminals.is_empty() {
                    self.new_terminal()?;
                } else if !self.studio.bottom_visible {
                    self.studio.bottom_visible = true;
                    self.studio.terminal_focus = true;
                    self.studio.bottom_plugin = false;
                } else if !self.studio.terminal_focus {
                    self.studio.terminal_focus = true;
                    self.studio.bottom_plugin = false;
                } else {
                    self.studio.terminal_focus = false;
                    self.studio.bottom_visible = false;
                }
            }
            "terminal.new" => self.new_terminal()?,
            "terminal.stop" => {
                if let Some(terminal) = self.studio.terminal_mut() {
                    terminal.stop();
                }
            }
            "terminal.close" => {
                if self.studio.terminal_active < self.studio.terminals.len() {
                    self.studio.terminals.remove(self.studio.terminal_active);
                }
                self.studio.terminal_active = self
                    .studio
                    .terminal_active
                    .min(self.studio.terminals.len().saturating_sub(1));
                if self.studio.terminals.is_empty() {
                    self.studio.bottom_visible = false;
                    self.studio.terminal_focus = false;
                }
            }
            "terminal.grow" => {
                self.studio.bottom_height = self.studio.bottom_height.saturating_add(3).min(60)
            }
            "terminal.shrink" => {
                self.studio.bottom_height = self.studio.bottom_height.saturating_sub(3).max(5)
            }
            "layout.explorer.grow" => {
                self.studio.explorer_width =
                    Some(self.studio.explorer_width.unwrap_or(27).saturating_add(3));
            }
            "layout.explorer.shrink" => {
                self.studio.explorer_width = Some(
                    self.studio
                        .explorer_width
                        .unwrap_or(27)
                        .saturating_sub(3)
                        .max(12),
                );
            }
            "layout.split.grow" => {
                self.studio.split_percent = self.studio.split_percent.saturating_add(5).min(95);
            }
            "layout.split.shrink" => {
                self.studio.split_percent = self.studio.split_percent.saturating_sub(5).max(5);
            }
            "layout.reset" => {
                self.studio.explorer_width = None;
                self.studio.split_percent = 50;
                self.studio.bottom_height = 12;
            }
            "cargo.check" | "cargo.build" | "cargo.run" | "cargo.test" | "cargo.clippy" => {
                self.cargo_task(command.trim_start_matches("cargo."))?
            }
            "cargo.package" | "cargo.target" => {
                let root = workspace::cargo_root(&self.root, self.doc().path.as_deref())?;
                let kind = command.to_owned();
                self.job(&kind, move || {
                    Ok(JobResult::Metadata(workspace::metadata(&root)?))
                })?;
            }
            "cargo.profile" => {
                self.studio.release = !self.studio.release;
                self.status = if self.studio.release {
                    "release"
                } else {
                    "debug"
                }
                .into();
            }
            "cargo.args" => self.input("run_args", "cargo.args"),
            "cargo.locations" => self.tool_locations(),
            "split.next" => {
                self.studio.split = Some((self.active + 1) % self.documents.len());
                self.studio.split_preview = false;
                self.studio.split_mermaid = false;
                self.studio.split_right_focus = false;
            }
            "split.preview" => {
                self.studio.split = Some(self.active);
                self.studio.split_preview = true;
                self.studio.split_mermaid = false;
                self.studio.split_right_focus = false;
            }
            "split.diagram" => {
                self.studio.split_right_focus = false;
                self.open_diagram();
                if self.diagram.is_some() {
                    self.dialog = None;
                    self.studio.split = Some(self.active);
                    self.studio.split_mermaid = true;
                    self.studio.split_preview = false;
                    self.studio.diagram_revision = self.doc().revision;
                }
            }
            "split.close" => {
                self.studio.split = None;
                self.studio.split_mermaid = false;
            }
            "lsp.restart" => {
                self.studio.lsp = None;
                self.studio.lsp_attempted = false;
                self.studio.lsp_error = None;
            }
            "lsp.diagnostics" => self.show_diagnostics(),
            "lsp.rename" => self.input("rename", "lsp.rename"),
            "lsp.complete" | "lsp.hover" | "lsp.definition" | "lsp.references" | "lsp.actions" => {
                self.lsp_request(command, None)?
            }
            "git.status" | "git.stage" => {
                let root = self.root.clone();
                let kind = command.to_owned();
                self.job(&kind, move || {
                    let branch = workspace::git(&root, &["branch", "--show-current"])?;
                    let status = workspace::git(&root, &["status", "--porcelain=v1", "-z"])?;
                    Ok(JobResult::Git(branch, status))
                })?;
            }
            "git.diff" => {
                let root = self.root.clone();
                self.job("git.diff", move || {
                    Ok(JobResult::Text(workspace::git(
                        &root,
                        &["diff", "HEAD", "--"],
                    )?))
                })?;
            }
            "git.commit" => self.input("git_commit", "git.commit"),
            "settings" | "settings.project" => {
                save_config(&self.root, &self.studio.config)?;
                self.open(&self.root.join(".reditor/config.toml"))?;
            }
            "settings.user" => {
                let dir = crate::settings::user_dir()?;
                fs::create_dir_all(&dir)?;
                let path = dir.join("config.toml");
                if !path.exists() {
                    fs::write(&path, toml::to_string_pretty(&Config::default())?)?;
                }
                self.open(&path)?;
            }
            "settings.theme" => self.select(
                "theme",
                [
                    "base16-ocean.dark",
                    "base16-eighties.dark",
                    "InspiredGitHub",
                    "Solarized (dark)",
                    "Solarized (light)",
                ]
                .iter()
                .map(|name| Choice {
                    label: (*name).into(),
                    action: Action::Command(format!("theme:{name}")),
                })
                .collect(),
            ),
            "settings.indent" => self.input("indent", "settings.indent"),
            "settings.wrap" => {
                self.studio.config.wrap = !self.studio.config.wrap;
                save_config(&self.root, &self.studio.config)?;
            }
            "settings.reload" => {
                let config = crate::settings::load(&self.root)?;
                if config.rust_analyzer != self.studio.config.rust_analyzer {
                    self.studio.lsp = None;
                    self.studio.lsp_attempted = false;
                    self.studio.lsp_error = None;
                }
                self.studio.config = config;
                self.i18n = crate::i18n::I18n::new(self.studio.config.language);
            }
            "plugins.settings" => self.select(
                "plugin_settings",
                self.plugins
                    .plugins
                    .iter()
                    .map(|p| Choice {
                        label: format!("{} · API {}", p.manifest.name, p.manifest.api_version),
                        action: Action::Jump(p.manifest_path.clone(), 0, 0, false),
                    })
                    .collect(),
            ),
            "diagram.list" => {
                let blocks = crate::diagram::blocks(self.doc());
                self.select(
                    "diagram_list",
                    blocks
                        .iter()
                        .enumerate()
                        .map(|(i, b)| Choice {
                            label: format!(
                                "{} · {}",
                                i + 1,
                                b.source
                                    .lines()
                                    .find_map(|line| {
                                        let line = line.trim();
                                        line.strip_prefix("title:")
                                            .or_else(|| line.strip_prefix("title "))
                                            .map(str::trim)
                                    })
                                    .or_else(|| b.source.lines().find(|l| !l.trim().is_empty()))
                                    .unwrap_or("mermaid")
                            ),
                            action: Action::Diagram(i),
                        })
                        .collect(),
                );
            }
            "diagram.labels" => {
                let items = self
                    .diagram
                    .as_ref()
                    .and_then(|d| d.geometry.as_ref())
                    .map(|g| {
                        g.labels
                            .iter()
                            .enumerate()
                            .map(|(i, l)| Choice {
                                label: l.text.clone(),
                                action: Action::Command(format!("diagram.label:{i}")),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.select("diagram_labels", items);
            }
            "diagram.connections" => {
                let body = self
                    .diagram
                    .as_ref()
                    .map(|view| {
                        view.blocks[view.selected]
                            .source
                            .lines()
                            .filter(|line| {
                                line.contains("->")
                                    || line.contains("--")
                                    || line.contains("<|")
                                    || line.contains("||")
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                self.text_panel("diagram_connections", body);
            }
            "diagram.svg" | "diagram.png" => self.input(
                if command.ends_with("svg") {
                    "export_svg"
                } else {
                    "export_png"
                },
                command,
            ),
            "review.apply" => self.apply_review()?,
            "format" => self.format()?,
            _ if command.starts_with("theme:") => {
                self.studio.config.theme = command[6..].into();
                save_config(&self.root, &self.studio.config)?;
            }
            _ if command.starts_with("diagram.label:") => {
                let index: usize = command[14..].parse()?;
                if let Some(view) = self.diagram.as_mut() {
                    if let Some(label) = view.geometry.as_ref().and_then(|g| g.labels.get(index)) {
                        // Same natural scale as the terminal renderer.
                        let scale =
                            crate::diagram_text::natural_scale(view.geometry.as_ref().unwrap());
                        view.zoom = 100;
                        view.graphics = false;
                        view.pan_x = (label.x * scale).max(0.0) as u32;
                        view.pan_y = (label.y * scale).max(0.0) as u32;
                    }
                    self.dialog = Some(Dialog::Diagram);
                }
            }
            _ => {
                let plugin = self
                    .plugins
                    .commands()
                    .into_iter()
                    .find(|(_, id, _)| id == command);
                if let Some((index, id, _)) = plugin {
                    self.execute_plugin(index, &id)?;
                } else {
                    bail!("Unknown command: {command}");
                }
            }
        }
        Ok(())
    }
    pub fn new_terminal(&mut self) -> Result<()> {
        let program = std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "powershell.exe".into()
            } else {
                "/bin/sh".into()
            }
        });
        let args = if cfg!(windows) {
            vec!["-NoLogo".into()]
        } else {
            vec!["-i".into()]
        };
        let terminal = TerminalSession::spawn(
            format!(
                "{} {}",
                self.i18n.t("terminal"),
                self.studio.terminals.len() + 1
            ),
            &self.root,
            &program,
            &args,
        )?;
        self.push_terminal(terminal);
        Ok(())
    }
    pub(crate) fn push_terminal(&mut self, terminal: TerminalSession) {
        self.studio.terminals.push(terminal);
        self.studio.terminal_active = self.studio.terminals.len() - 1;
        self.studio.bottom_visible = true;
        self.studio.bottom_plugin = false;
        self.studio.terminal_focus = true;
    }
    fn cargo_task(&mut self, task: &str) -> Result<()> {
        let root = workspace::cargo_root(&self.root, self.doc().path.as_deref())?;
        let mut args = vec![task.into()];
        if let Some(package) = &self.studio.package {
            args.extend(["--package".into(), package.clone()]);
        }
        if self.studio.release {
            args.push("--release".into());
        }
        if task == "run" {
            if let Some(target) = &self.studio.target {
                args.extend(["--bin".into(), target.clone()]);
            }
            if !self.studio.run_args.trim().is_empty() {
                args.push("--".into());
                args.extend(shell_arguments(&self.studio.run_args)?);
            }
        }
        let title = format!(
            "cargo {}{}",
            task,
            if self.studio.release {
                " --release"
            } else {
                ""
            }
        );
        let terminal = TerminalSession::spawn(title, &root, "cargo", &args)?;
        self.push_terminal(terminal);
        if self.documents.iter().any(Document::dirty) {
            self.message("cargo_unsaved");
        }
        Ok(())
    }
    pub fn studio_key(&mut self, key: KeyEvent) -> Result<bool> {
        if matches!(self.dialog, Some(Dialog::Workbench)) {
            self.panel_key(key)?;
            return Ok(true);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if (control && matches!(key.code, KeyCode::Char('`'))) || key.code == KeyCode::F(10) {
            self.command("terminal.toggle")?;
            return Ok(true);
        }
        if self.studio.terminal_focus && self.dialog.is_none() {
            if key.modifiers.contains(KeyModifiers::ALT)
                && let KeyCode::Char(c @ '1'..='9') = key.code
            {
                self.studio.terminal_active =
                    (c as usize - '1' as usize).min(self.studio.terminals.len().saturating_sub(1));
                return Ok(true);
            }
            if !(control && key.code == KeyCode::Char('q')) {
                if let Some(terminal) = self.studio.terminal_mut() {
                    terminal.key(key)?;
                }
                return Ok(true);
            }
        }
        if self.dialog.is_some() {
            return Ok(false);
        }
        let binding = shortcut(key);
        if let Some(command) = self.studio.config.keybindings.get(&binding).cloned() {
            self.command(&command)?;
            return Ok(true);
        }
        let plugin = self.plugins.plugins.iter().enumerate().find_map(|(i, p)| {
            p.manifest
                .commands
                .iter()
                .find(|c| c.key.as_deref() == Some(binding.as_str()))
                .map(|c| (i, c.id.clone()))
        });
        if let Some((index, event)) = plugin {
            self.execute_plugin(index, &event)?;
            return Ok(true);
        }
        let action = match binding.as_str() {
            "Ctrl+Shift+p" => Some("palette"),
            "Ctrl+p" => Some("files"),
            "Ctrl+Alt+p" => Some("projects"),
            "Ctrl+Alt+o" => Some("project.open"),
            "Ctrl+g" => Some("palette"),
            "Ctrl+Shift+o" => Some("files"),
            "Ctrl+Shift+f" => Some("search.project"),
            "Ctrl+h" => Some("replace.file"),
            "Ctrl+Shift+h" => Some("replace.project"),
            "Ctrl+Space" => Some("lsp.complete"),
            "F12" => Some("lsp.definition"),
            "Shift+F12" => Some("lsp.references"),
            "Ctrl+k" => Some("lsp.hover"),
            "Ctrl+j" => Some("palette"),
            "Ctrl+Shift+r" => Some("lsp.rename"),
            "Alt+Enter" => Some("lsp.actions"),
            "Ctrl+\\" => Some("split.next"),
            "F11" => Some("cargo.run"),
            "Ctrl+F11" => Some("cargo.build"),
            "Ctrl+Shift+b" => Some("cargo.build"),
            "Ctrl+Shift+t" => Some("cargo.test"),
            _ => None,
        };
        if let Some(action) = action {
            self.command(action)?;
            return Ok(true);
        }
        if self.studio.config.wrap
            && !self.doc().preview
            && !self.explorer_focus
            && !control
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                let old = self.doc().cursor;
                self.doc_mut().anchor.get_or_insert(old);
            } else {
                self.doc_mut().anchor = None;
            }
            let width = self.studio.wrap_width.max(1);
            let (row, _) = self.doc().position();
            let visual = self.doc().visual_column();
            let (target, column) = if key.code == KeyCode::Up {
                if visual >= width {
                    (row, visual - width)
                } else if row > 0 {
                    let end = self.doc().line(row - 1).width();
                    (row - 1, end.saturating_sub(1) / width * width + visual)
                } else {
                    (row, visual)
                }
            } else {
                let end = self.doc().line(row).width();
                if visual + width <= end {
                    (row, visual + width)
                } else if row + 1 < self.doc().text.len_lines() {
                    (row + 1, visual % width)
                } else {
                    (row, visual)
                }
            };
            self.doc_mut().cursor = self.doc().cursor_at(target, column);
            return Ok(true);
        }
        Ok(false)
    }
    fn panel_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.code == KeyCode::Esc {
            self.dialog = None;
            self.studio.panel = None;
            if let Some(job) = self.studio.job.as_mut() {
                job.display = false;
            }
            return Ok(());
        }
        let Some(mut panel) = self.studio.panel.take() else {
            self.dialog = None;
            return Ok(());
        };
        let filtered = panel.filtered();
        if matches!(panel.mode, PanelMode::Text | PanelMode::Review) {
            panel.scroll = match key.code {
                KeyCode::Up => panel.scroll.saturating_sub(1),
                KeyCode::Down => panel.scroll.saturating_add(1),
                KeyCode::PageUp => panel.scroll.saturating_sub(10),
                KeyCode::PageDown => panel.scroll.saturating_add(10),
                KeyCode::Home => 0,
                KeyCode::End => panel.body.lines().count().saturating_sub(1),
                _ => panel.scroll,
            };
            if key.code != KeyCode::Enter {
                self.studio.panel = Some(panel);
                return Ok(());
            }
        }
        match key.code {
            KeyCode::Up => panel.selected = panel.selected.saturating_sub(1),
            KeyCode::Down => panel.selected = panel.selected.saturating_add(1),
            KeyCode::PageUp => panel.selected = panel.selected.saturating_sub(10),
            KeyCode::PageDown => panel.selected = panel.selected.saturating_add(10),
            KeyCode::Home => panel.selected = 0,
            KeyCode::Backspace => {
                panel.input.pop();
                panel.selected = 0;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                panel.input.clear();
                panel.selected = 0;
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                panel.input.push(c);
                panel.selected = 0;
            }
            KeyCode::Enter => {
                self.dialog = None;
                let outcome = match panel.mode.clone() {
                    PanelMode::Input(kind) => self.panel_submit(&kind, &panel.input),
                    PanelMode::Select | PanelMode::Review => {
                        if let Some(index) = filtered.get(panel.selected) {
                            self.choose(panel.items[*index].action.clone())
                        } else {
                            Ok(())
                        }
                    }
                    PanelMode::Text => {
                        self.dialog = Some(Dialog::Workbench);
                        self.studio.panel = Some(panel);
                        return Ok(());
                    }
                };
                if outcome.is_err() && self.studio.panel.is_none() {
                    self.studio.panel = Some(panel);
                    self.dialog = Some(Dialog::Workbench);
                }
                return outcome;
            }
            _ => {}
        }
        if !matches!(panel.mode, PanelMode::Text) {
            panel.selected = panel.selected.min(panel.filtered().len().saturating_sub(1));
        }
        self.studio.panel = Some(panel);
        Ok(())
    }
    fn choose(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Command(command) => self.command(&command)?,
            Action::Plugin(index, event) => self.execute_plugin(index, &event)?,
            Action::PluginMenu(index) => self.show_plugin_commands(index)?,
            Action::Project(path) => self.switch_project(&path)?,
            Action::RemoteBrowse(id, path) => self.remote_browse(id, path)?,
            Action::RemoteOpen(id, path) => self.remote_open(id, path)?,
            Action::Jump(path, line, column, utf16) => {
                self.open(&path)?;
                self.doc_mut().preview = false;
                let cursor = if utf16 {
                    lsp::offset(&self.doc().text, &json!({"line":line,"character":column}))?
                } else {
                    self.doc()
                        .text
                        .line_to_char(line.min(self.doc().text.len_lines() - 1))
                        + column.min(
                            self.doc()
                                .line(line.min(self.doc().text.len_lines() - 1))
                                .chars()
                                .count(),
                        )
                };
                self.doc_mut().cursor = cursor;
                self.doc_mut().anchor = None;
            }
            Action::Complete(item) => self.apply_completion(item)?,
            Action::Fix(value) => {
                if let Some(edit) = value.get("edit") {
                    self.review_workspace(edit)?;
                } else if value.get("data").is_some() {
                    self.studio
                        .lsp
                        .as_mut()
                        .context("LSP unavailable")?
                        .request("codeAction/resolve", value, "resolve")?;
                } else {
                    let command = value.get("command").unwrap_or(&value);
                    self.studio.lsp.as_mut().context("LSP unavailable")?.request("workspace/executeCommand", json!({ "command": command["command"], "arguments": command["arguments"] }), "execute")?;
                }
            }
            Action::Package(package) => {
                self.studio.package = (!package.is_empty()).then_some(package);
                self.studio.target = None;
            }
            Action::Target(target) => self.studio.target = (!target.is_empty()).then_some(target),
            Action::GitDiff(path) => {
                let root = self.root.clone();
                self.job("git.diff", move || {
                    Ok(JobResult::Text(workspace::git_diff(&root, &path)?))
                })?;
            }
            Action::Stage(path) => {
                let root = self.root.clone();
                self.job("git.stage.done", move || {
                    Ok(JobResult::Text(workspace::git(
                        &root,
                        &[
                            "add",
                            "--",
                            path.to_str().context("UTF-8 Git path required")?,
                        ],
                    )?))
                })?;
            }
            Action::Diagram(index) => {
                let blocks = crate::diagram::blocks(self.doc());
                if index < blocks.len() {
                    let mut view = crate::diagram::DiagramView::new(blocks, index);
                    view.start();
                    self.diagram = Some(view);
                    self.dialog = Some(Dialog::Diagram);
                }
            }
        }
        Ok(())
    }
    fn panel_submit(&mut self, kind: &str, input: &str) -> Result<()> {
        match kind {
            "project.open" => {
                let path = PathBuf::from(input);
                let path = if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                };
                self.switch_project(&path)?;
            }
            "remote.mkdir" | "remote.new_file" => self.remote_input(kind, input)?,
            "search.project" => {
                let root = self.root.clone();
                let needle = input.to_owned();
                self.job(kind, move || {
                    Ok(JobResult::Hits(workspace::search(&root, &needle)))
                })?;
            }
            "replace.find" => {
                anyhow::ensure!(!input.is_empty(), "Empty search");
                self.studio.needle = input.into();
                self.input("replace_with", "replace.with");
            }
            "replace.with" => {
                let needle = self.studio.needle.clone();
                let mut changes = vec![];
                if self.studio.project_replace {
                    let root = self.root.clone();
                    let replacement = input.to_owned();
                    let open: HashMap<PathBuf, (u64, String)> = self
                        .documents
                        .iter()
                        .filter_map(|d| {
                            d.path
                                .as_ref()
                                .map(|path| (path.clone(), (d.id, d.text.to_string())))
                        })
                        .collect();
                    return self.job("replace.project", move || {
                        let mut changes = vec![];
                        for path in workspace::files(&root) {
                            let opened = open.get(&path);
                            let before = if let Some((_, text)) = opened {
                                text.clone()
                            } else {
                                if fs::metadata(&path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
                                    continue;
                                }
                                match fs::read_to_string(&path) {
                                    Ok(s) if !s.contains('\0') => s,
                                    _ => continue,
                                }
                            };
                            if before.contains(&needle) {
                                changes.push(Change {
                                    path: Some(path),
                                    id: opened.map(|(id, _)| *id),
                                    after: before.replace(&needle, &replacement),
                                    before,
                                });
                            }
                        }
                        Ok(JobResult::Changes(changes))
                    });
                }
                let before = self.doc().text.to_string();
                if before.contains(&needle) {
                    changes.push(Change {
                        path: self.doc().path.clone(),
                        id: Some(self.doc().id),
                        after: before.replace(&needle, input),
                        before,
                    });
                }
                self.review(changes);
            }
            "lsp.rename" => self.lsp_request("lsp.rename", Some(input))?,
            "cargo.args" => {
                shell_arguments(input)?;
                self.studio.run_args = input.into();
            }
            "settings.indent" => {
                let indent: usize = input.parse()?;
                anyhow::ensure!((1..=16).contains(&indent), "Indent must be 1..16");
                self.studio.config.indent = indent;
                save_config(&self.root, &self.studio.config)?;
            }
            "git.commit" => {
                anyhow::ensure!(!input.trim().is_empty(), "Empty commit message");
                let terminal = TerminalSession::spawn(
                    "git commit".into(),
                    &self.root,
                    "git",
                    &["commit".into(), "-m".into(), input.into()],
                )?;
                self.push_terminal(terminal);
            }
            "diagram.svg" | "diagram.png" => {
                let view = self
                    .diagram
                    .as_ref()
                    .context("Open a diagram with F9 first")?;
                let data = if kind.ends_with("svg") {
                    view.svg
                        .as_ref()
                        .context("Diagram is still loading")?
                        .as_bytes()
                        .to_vec()
                } else {
                    let image = view.image.as_ref().context("Diagram is still loading")?;
                    let mut buffer = std::io::Cursor::new(Vec::new());
                    image.write_to(&mut buffer, image::ImageFormat::Png)?;
                    buffer.into_inner()
                };
                let path = if Path::new(input).is_absolute() {
                    PathBuf::from(input)
                } else {
                    self.root.join(input)
                };
                // create_new protects existing exports; choose a new filename when replacing.
                self.job("export", move || {
                    let mut f = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    f.write_all(&data)?;
                    Ok(JobResult::Export(path))
                })?;
            }
            _ => bail!("Unknown input: {kind}"),
        }
        Ok(())
    }
    fn review(&mut self, changes: Vec<Change>) {
        let mut body = String::new();
        for change in &changes {
            body.push_str(&format!(
                "\n{}\n",
                change
                    .path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| self.i18n.t("untitled").into())
            ));
            let before: Vec<&str> = change.before.split_inclusive('\n').collect();
            let after: Vec<&str> = change.after.split_inclusive('\n').collect();
            let prefix = before
                .iter()
                .zip(&after)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = before[prefix..]
                .iter()
                .rev()
                .zip(after[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            for (index, line) in before
                .iter()
                .enumerate()
                .take(before.len() - suffix)
                .skip(prefix)
            {
                body.push_str(&format!("{} - {}", index + 1, line));
                if !line.ends_with('\n') {
                    body.push('\n');
                }
            }
            for (index, line) in after
                .iter()
                .enumerate()
                .take(after.len() - suffix)
                .skip(prefix)
            {
                body.push_str(&format!("{} + {}", index + 1, line));
                if !line.ends_with('\n') {
                    body.push('\n');
                }
            }
        }
        self.studio.changes = changes;
        self.studio_panel(
            self.i18n.t("review").into(),
            PanelMode::Review,
            body,
            vec![Choice {
                label: self.i18n.t("apply_buffers").into(),
                action: Action::Command("review.apply".into()),
            }],
        );
    }
    fn apply_review(&mut self) -> Result<()> {
        // Validate every original before mutating any buffer.
        for change in &self.studio.changes {
            let current = if let Some(d) = self.documents.iter().find(|d| {
                change.id == Some(d.id) || (change.path.is_some() && d.path == change.path)
            }) {
                d.text.to_string()
            } else {
                fs::read_to_string(change.path.as_ref().context("Closed unsaved document")?)?
            };
            anyhow::ensure!(current == change.before, "Document changed since review");
        }
        let mut opened = vec![];
        for change in &self.studio.changes {
            if !self.documents.iter().any(|d| {
                change.id == Some(d.id) || (change.path.is_some() && d.path == change.path)
            }) {
                let doc = Document::open(change.path.as_ref().context("Missing path")?)?;
                anyhow::ensure!(doc.text == change.before, "File changed during review");
                opened.push(doc);
            }
        }
        self.documents.extend(opened);
        let changes = std::mem::take(&mut self.studio.changes);
        for change in changes {
            if let Some(index) = self.documents.iter().position(|d| {
                change.id == Some(d.id) || (change.path.is_some() && d.path == change.path)
            }) {
                self.active = index;
            } else {
                self.open(change.path.as_ref().context("Missing path")?)?;
            }
            self.doc_mut().replace(&change.after);
        }
        self.message("applied_unsaved");
        Ok(())
    }
    fn lsp_request(&mut self, command: &str, input: Option<&str>) -> Result<()> {
        let path = self
            .doc()
            .path
            .clone()
            .context("Save the Rust document first")?;
        anyhow::ensure!(
            path.extension().is_some_and(|e| e == "rs"),
            "rust-analyzer requires a Rust file"
        );
        self.doc_mut().preview = false;
        let uri = lsp::uri(&path)?;
        let position = lsp::position(self.doc());
        self.studio.completion_origin =
            Some((self.doc().id, self.doc().revision, self.doc().cursor));
        self.studio.lsp_origin = Some((self.doc().id, self.doc().revision));
        let documents = &self.documents;
        let client = self.studio.lsp.as_mut().context(
            "rust-analyzer unavailable; install with rustup component add rust-analyzer",
        )?;
        client.sync(documents)?;
        let mut params = json!({"textDocument":{"uri":uri},"position":position});
        let method = match command {
            "lsp.complete" => {
                params["context"] = json!({"triggerKind":1});
                "textDocument/completion"
            }
            "lsp.hover" => "textDocument/hover",
            "lsp.definition" => "textDocument/definition",
            "lsp.references" => {
                params["context"] = json!({"includeDeclaration":true});
                "textDocument/references"
            }
            "lsp.rename" => {
                params["newName"] = json!(input.context("Missing new name")?);
                "textDocument/rename"
            }
            "lsp.actions" => {
                params["range"] = json!({"start":position,"end":position});
                params["context"] = json!({"diagnostics":client.diagnostics.get(&uri).cloned().unwrap_or_default()});
                "textDocument/codeAction"
            }
            _ => bail!("Unknown LSP request"),
        };
        client.request(method, params, command)?;
        self.message("running");
        Ok(())
    }
    fn apply_completion(&mut self, item: Value) -> Result<()> {
        anyhow::ensure!(
            self.studio.completion_origin
                == Some((self.doc().id, self.doc().revision, self.doc().cursor)),
            "Document changed since completion request"
        );
        anyhow::ensure!(
            item["insertTextFormat"].as_u64() != Some(2),
            "Server returned an unsupported snippet"
        );
        let mut edits = item["additionalTextEdits"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(edit) = item.get("textEdit") {
            edits.push(json!({"range":edit.get("range").or_else(||edit.get("replace")).context("Missing completion range")?,"newText":edit["newText"]}));
        } else {
            let mut start = self.doc().cursor;
            while start > 0 && {
                let c = self.doc().text.char(start - 1);
                c.is_alphanumeric() || c == '_'
            } {
                start -= 1;
            }
            let mut position = lsp::position(self.doc());
            let prefix = self
                .doc()
                .text
                .slice(start..self.doc().cursor)
                .to_string()
                .encode_utf16()
                .count();
            position["character"] = json!(
                position["character"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(prefix as u64)
            );
            edits.push(json!({"range":{"start":position,"end":lsp::position(self.doc())},"newText":item.get("insertText").unwrap_or(&item["label"])}));
        }
        let updated = lsp::edits(&self.doc().text.to_string(), &edits)?;
        let main = edits.last().context("Missing completion edit")?;
        let start = lsp::offset(&self.doc().text, &main["range"]["start"])?;
        let inserted = main["newText"].as_str().unwrap_or("").chars().count();
        let mut cursor = start + inserted;
        for edit in edits.iter().take(edits.len().saturating_sub(1)) {
            let begin = lsp::offset(&self.doc().text, &edit["range"]["start"])?;
            let end = lsp::offset(&self.doc().text, &edit["range"]["end"])?;
            if begin < start {
                cursor = cursor.saturating_add_signed(
                    edit["newText"].as_str().unwrap_or("").chars().count() as isize
                        - (end - begin) as isize,
                );
            }
        }
        self.doc_mut().replace(&updated);
        self.doc_mut().cursor = cursor.min(self.doc().text.len_chars());
        Ok(())
    }
    fn review_workspace(&mut self, edit: &Value) -> Result<()> {
        let mut edits: HashMap<PathBuf, Vec<Value>> = HashMap::new();
        if let Some(changes) = edit["changes"].as_object() {
            for (uri, list) in changes {
                edits.insert(
                    lsp::path(uri)?,
                    list.as_array().context("Missing edits")?.clone(),
                );
            }
        }
        if let Some(changes) = edit["documentChanges"].as_array() {
            for change in changes {
                anyhow::ensure!(
                    change.get("kind").is_none(),
                    "File create/rename/delete actions are not supported"
                );
                let path = lsp::path(
                    change["textDocument"]["uri"]
                        .as_str()
                        .context("Missing edit URI")?,
                )?;
                if let Some(version) = change["textDocument"]["version"].as_u64() {
                    let d = self
                        .documents
                        .iter()
                        .find(|d| d.path.as_ref() == Some(&path))
                        .context("Versioned document not open")?;
                    anyhow::ensure!(d.revision == version, "Stale LSP edit");
                }
                edits
                    .entry(path)
                    .or_default()
                    .extend(change["edits"].as_array().context("Missing edits")?.clone());
            }
        }
        let mut changes = vec![];
        for (path, edits) in edits {
            let open = self
                .documents
                .iter()
                .find(|d| d.path.as_ref() == Some(&path));
            let before = if let Some(d) = open {
                d.text.to_string()
            } else {
                fs::read_to_string(&path)?
            };
            changes.push(Change {
                after: lsp::edits(&before, &edits)?,
                before,
                path: Some(path),
                id: open.map(|d| d.id),
            });
        }
        self.review(changes);
        Ok(())
    }
    fn show_diagnostics(&mut self) {
        let mut items = vec![];
        if let Some(client) = &self.studio.lsp {
            for (uri, diagnostics) in &client.diagnostics {
                if let Ok(path) = lsp::path(uri) {
                    for d in diagnostics {
                        let row = d["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
                        let column =
                            d["range"]["start"]["character"].as_u64().unwrap_or(0) as usize;
                        items.push(Choice {
                            label: format!(
                                "{}:{} {}",
                                path.display(),
                                row + 1,
                                d["message"].as_str().unwrap_or("")
                            ),
                            action: Action::Jump(path.clone(), row, column, true),
                        });
                    }
                }
            }
        }
        if items.is_empty() {
            self.text_panel(
                "diagnostics",
                self.studio
                    .lsp_error
                    .clone()
                    .unwrap_or_else(|| self.i18n.t("no_results").into()),
            );
        } else {
            self.select("diagnostics", items);
        }
    }
    fn tool_locations(&mut self) {
        let root = self.root.clone();
        let mut items = vec![];
        if let Some(terminal) = self.studio.terminals.get(self.studio.terminal_active) {
            // rustc emits `--> path:line:column`; parse from the right for Windows paths.
            for line in terminal.output.lines() {
                if let Some((_, location)) = line.split_once("--> ") {
                    let mut parts = location.trim().rsplitn(3, ':');
                    if let (Some(col), Some(row), Some(path)) =
                        (parts.next(), parts.next(), parts.next())
                        && let (Ok(row), Ok(col)) = (row.parse::<usize>(), col.parse::<usize>())
                    {
                        items.push(Choice {
                            label: location.into(),
                            action: Action::Jump(
                                root.join(path),
                                row.saturating_sub(1),
                                col.saturating_sub(1),
                                false,
                            ),
                        });
                    }
                }
            }
        }
        self.select("tool_locations", items);
    }
    pub fn poll_studio(&mut self) {
        self.proofread_auto();
        if let Some(result) = self
            .studio
            .proofread_job
            .as_ref()
            .and_then(|job| job.try_recv().ok())
        {
            self.studio.proofread_job = None;
            match result {
                Ok((id, revision, report)) => {
                    if let Err(error) = self.proofread_result(id, revision, report, false) {
                        self.error(error);
                    }
                }
                Err(error) => self.error(error),
            }
        }
        if !self.job_running()
            && let Some((path, text)) = self.studio.pending_uploads.pop_front()
            && let Err(error) = self.remote_upload_saved(path, text)
        {
            self.error(error);
        }
        for terminal in &mut self.studio.terminals {
            terminal.poll();
        }
        if self.studio.git_job.is_none() && self.studio.last_git.elapsed() > Duration::from_secs(3)
        {
            self.studio.last_git = Instant::now();
            let root = self.root.clone();
            let (send, receive) = mpsc::channel();
            thread::spawn(move || {
                let _ = send.send(workspace::git_snapshot(&root));
            });
            self.studio.git_job = Some(receive);
        }
        if let Some(result) = self.studio.git_job.as_ref().and_then(|r| r.try_recv().ok()) {
            self.studio.git_job = None;
            if let Ok((branch, lines)) = result {
                self.studio.branch = branch;
                self.studio.git_lines = lines;
            }
        }
        if self.studio.last_session.elapsed() > Duration::from_secs(2) {
            self.studio.last_session = Instant::now();
            if let Err(e) = session::save(&self.root, &self.documents, self.active) {
                self.error(e);
            }
        }
        if !self.studio.lsp_attempted
            && self.documents.iter().any(|d| {
                d.path
                    .as_ref()
                    .is_some_and(|p| p.extension().is_some_and(|e| e == "rs"))
            })
        {
            self.studio.lsp_attempted = true;
            if !self.studio.config.rust_analyzer.is_empty() {
                match lsp::Client::start(&self.root, &self.studio.config.rust_analyzer) {
                    Ok(client) => self.studio.lsp = Some(client),
                    Err(e) => self.studio.lsp_error = Some(e.to_string()),
                }
            }
        }
        let events = if let Some(client) = self.studio.lsp.as_mut() {
            let events = client.poll();
            if self.studio.last_lsp.elapsed() > Duration::from_millis(300) {
                self.studio.last_lsp = Instant::now();
                let _ = client.sync(&self.documents);
            }
            events
        } else {
            vec![]
        };
        for event in events {
            if let Err(e) = self.lsp_event(event) {
                self.error(e);
            }
        }
        if self.studio.split_mermaid
            && self.studio.split == Some(self.active)
            && self.studio.diagram_revision != self.doc().revision
            && self.studio.last_diagram.elapsed() > Duration::from_millis(600)
        {
            self.studio.last_diagram = Instant::now();
            self.studio.diagram_revision = self.doc().revision;
            let selected = self.diagram.as_ref().map(|d| d.selected).unwrap_or(0);
            let blocks = crate::diagram::blocks(self.doc());
            if !blocks.is_empty() {
                let mut view = crate::diagram::DiagramView::new(
                    blocks.clone(),
                    selected.min(blocks.len() - 1),
                );
                view.start();
                self.diagram = Some(view);
            }
        }
        let ready = self
            .studio
            .job
            .as_ref()
            .and_then(|job| job.receive.try_recv().ok());
        if let Some(result) = ready {
            let job = self.studio.job.take().unwrap();
            if !job.display {
                // A completed upload must update its baseline even after Esc.
                match result {
                    Ok(JobResult::Remote(outcome))
                        if matches!(*outcome, crate::remote::Outcome::Uploaded(_, _)) =>
                    {
                        let dialog = self.dialog.take();
                        if let Err(error) = self.remote_result(*outcome) {
                            self.error(error);
                        }
                        self.dialog = dialog;
                    }
                    Ok(JobResult::Proofread {
                        id,
                        revision,
                        report,
                        ..
                    }) => {
                        if let Err(error) = self.proofread_result(id, revision, report, false) {
                            self.error(error);
                        }
                    }
                    Err(error) if job.kind.starts_with("remote.") => self.error(error),
                    _ => {}
                }
                return;
            }
            let kind = job.kind;
            if let Err(e) = result.and_then(|result| self.job_result(&kind, result)) {
                self.error(e);
                self.text_panel("error", self.status.clone());
            }
        }
    }
    fn lsp_event(&mut self, event: lsp::Event) -> Result<()> {
        match event {
            lsp::Event::Error(error) => {
                self.studio.lsp_error = Some(error);
            }
            lsp::Event::Edit(edit) => self.review_workspace(&edit)?,
            lsp::Event::Response(kind, result) => match kind.as_str() {
                "lsp.complete" => {
                    let entries = result
                        .as_array()
                        .or_else(|| result["items"].as_array())
                        .cloned()
                        .unwrap_or_default();
                    self.select(
                        "completion",
                        entries
                            .into_iter()
                            .map(|item| Choice {
                                label: format!(
                                    "{} {}",
                                    item["label"].as_str().unwrap_or(""),
                                    item["detail"].as_str().unwrap_or("")
                                ),
                                action: Action::Complete(item),
                            })
                            .collect(),
                    );
                }
                "lsp.hover" => {
                    let value = &result["contents"];
                    let body = value["value"]
                        .as_str()
                        .or_else(|| value.as_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    self.text_panel("hover", body);
                }
                "lsp.definition" | "lsp.references" => {
                    let entries = result.as_array().cloned().unwrap_or_else(|| {
                        if result.is_object() {
                            vec![result]
                        } else {
                            vec![]
                        }
                    });
                    let mut items = vec![];
                    for item in entries {
                        let uri = item["uri"].as_str().or_else(|| item["targetUri"].as_str());
                        if let Some(uri) = uri {
                            let range = item
                                .get("range")
                                .or_else(|| item.get("targetSelectionRange"))
                                .context("Missing location range")?;
                            let row = range["start"]["line"].as_u64().unwrap_or(0) as usize;
                            let col = range["start"]["character"].as_u64().unwrap_or(0) as usize;
                            let path = lsp::path(uri)?;
                            items.push(Choice {
                                label: format!("{}:{}:{}", path.display(), row + 1, col + 1),
                                action: Action::Jump(path, row, col, true),
                            });
                        }
                    }
                    if items.len() == 1 {
                        self.choose(items.remove(0).action)?;
                    } else {
                        self.select("references", items);
                    }
                }
                "lsp.rename" => {
                    anyhow::ensure!(
                        self.studio.lsp_origin == Some((self.doc().id, self.doc().revision)),
                        "Document changed since rename request"
                    );
                    self.review_workspace(&result)?;
                }
                "lsp.actions" => self.select(
                    "code_actions",
                    result
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|item| Choice {
                            label: item["title"].as_str().unwrap_or("action").into(),
                            action: Action::Fix(item),
                        })
                        .collect(),
                ),
                "resolve" => self.choose(Action::Fix(result))?,
                _ => {}
            },
        }
        Ok(())
    }
    fn job_result(&mut self, kind: &str, result: JobResult) -> Result<()> {
        match result {
            JobResult::Remote(outcome) => self.remote_result(*outcome)?,
            JobResult::Proofread {
                id,
                revision,
                report,
                show_panel,
            } => self.proofread_result(id, revision, report, show_panel)?,
            JobResult::Formatted {
                id,
                revision,
                before,
                after,
                check,
            } => self.formatter_result(id, revision, before, after, check)?,
            JobResult::Files(paths) => self.select(
                "quick_open",
                paths
                    .into_iter()
                    .map(|path| Choice {
                        label: path
                            .strip_prefix(&self.root)
                            .unwrap_or(&path)
                            .display()
                            .to_string(),
                        action: Action::Jump(path, 0, 0, false),
                    })
                    .collect(),
            ),
            JobResult::Hits(hits) => self.select(
                "project_search",
                hits.into_iter()
                    .map(|hit| Choice {
                        label: format!(
                            "{}:{} {}",
                            hit.path
                                .strip_prefix(&self.root)
                                .unwrap_or(&hit.path)
                                .display(),
                            hit.line + 1,
                            hit.text
                        ),
                        action: Action::Jump(hit.path, hit.line, hit.column, false),
                    })
                    .collect(),
            ),
            JobResult::Metadata(value) => {
                let members = value["workspace_members"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let packages = value["packages"].as_array().cloned().unwrap_or_default();
                let mut items = vec![];
                if kind == "cargo.package" {
                    items.push(Choice {
                        label: self.i18n.t("cargo_default").into(),
                        action: Action::Package(String::new()),
                    });
                } else {
                    items.push(Choice {
                        label: self.i18n.t("cargo_default").into(),
                        action: Action::Target(String::new()),
                    });
                }
                for package in packages {
                    if !members.contains(&package["id"]) {
                        continue;
                    }
                    let name = package["name"].as_str().unwrap_or("");
                    if kind == "cargo.package" {
                        items.push(Choice {
                            label: name.into(),
                            action: Action::Package(name.into()),
                        });
                    } else if self.studio.package.as_deref().is_none_or(|p| p == name) {
                        for target in package["targets"].as_array().into_iter().flatten() {
                            if target["kind"]
                                .as_array()
                                .is_some_and(|k| k.contains(&json!("bin")))
                            {
                                let name = target["name"].as_str().unwrap_or("");
                                items.push(Choice {
                                    label: name.into(),
                                    action: Action::Target(name.into()),
                                });
                            }
                        }
                    }
                }
                self.select(
                    if kind == "cargo.package" {
                        "cargo_package"
                    } else {
                        "cargo_target"
                    },
                    items,
                );
            }
            JobResult::Git(branch, status) => {
                self.studio.branch = branch.trim().into();
                let mut items = vec![];
                let mut records = status.split('\0');
                while let Some(record) = records.next() {
                    if record.len() < 4 {
                        continue;
                    }
                    let flags = &record[..2];
                    let path = self.root.join(&record[3..]);
                    let action = if kind == "git.stage" {
                        Action::Stage(path.clone())
                    } else {
                        Action::GitDiff(path.clone())
                    };
                    items.push(Choice {
                        label: format!(
                            "{flags} {}",
                            path.strip_prefix(&self.root).unwrap_or(&path).display()
                        ),
                        action,
                    });
                    if flags.contains('R') || flags.contains('C') {
                        records.next();
                    }
                }
                self.select("git_status", items);
            }
            JobResult::Text(text) => self.text_panel(
                if kind.starts_with("git") {
                    "git_diff"
                } else {
                    "output"
                },
                text,
            ),
            JobResult::Export(path) => {
                self.status = format!("{} {}", self.i18n.t("saved"), path.display());
                self.dialog = None;
            }
            JobResult::Changes(changes) => self.review(changes),
        }
        Ok(())
    }
}
pub fn shell_arguments(text: &str) -> Result<Vec<String>> {
    // Parse argv without evaluating shell substitutions or running a shell.
    let mut args = vec![];
    let mut current = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut started = false;
    for c in text.chars() {
        if escape {
            current.push(c);
            escape = false;
            started = true;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escape = true;
            started = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                current.push(c);
            }
            continue;
        }
        if matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                args.push(std::mem::take(&mut current));
                started = false;
            }
        } else {
            current.push(c);
            started = true;
        }
    }
    anyhow::ensure!(quote.is_none() && !escape, "Unclosed argument quote");
    if started {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_shortcuts_open_files_commands_and_projects() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("example.rs"), "fn main() {}")?;
        let mut app = App::new(root.path().into(), Language::Ru, vec![])?;
        app.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.studio.job.is_some() && Instant::now() < deadline {
            app.poll_studio();
            thread::sleep(Duration::from_millis(10));
        }
        let panel = app
            .studio
            .panel
            .as_ref()
            .context("File panel did not open")?;
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label.contains("example.rs"))
        );
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.key(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert!(
            app.studio
                .panel
                .as_ref()
                .is_some_and(|panel| panel.title == app.i18n.t("palette"))
        );
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert!(app.studio.panel.as_ref().is_some_and(|panel| {
            panel
                .items
                .iter()
                .any(|item| item.label.contains(root.path().to_str().unwrap()))
        }));
        Ok(())
    }
    #[test]
    fn argument_parsing_never_executes_shell_text() -> Result<()> {
        assert_eq!(
            shell_arguments("'Привет мир' \"\" '$(id)' --flag")?,
            vec!["Привет мир", "", "$(id)", "--flag"]
        );
        assert!(shell_arguments("'broken").is_err());
        Ok(())
    }
    #[test]
    fn plugin_panel_and_diagram_exports_preserve_existing_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/panel-plugin");
        let mut app = App::new(root.path().into(), Language::Ru, vec![plugin])?;
        assert_eq!(app.plugins.plugins[0].manifest.name, "Statistics");
        app.execute_plugin(0, "statistics")?;
        assert!(app.studio.bottom_plugin);
        assert!(
            app.studio
                .plugin_panel
                .as_ref()
                .unwrap()
                .content
                .contains("API: 1")
        );
        let mut view = crate::diagram::DiagramView::new(
            vec![crate::diagram::DiagramBlock {
                source: "flowchart LR\n A-->B".into(),
                start: 0,
                end: 0,
            }],
            0,
        );
        view.svg = Some("<svg xmlns='http://www.w3.org/2000/svg'><text>Привет</text></svg>".into());
        view.image = Some(image::DynamicImage::new_rgb8(4, 4));
        app.diagram = Some(view);
        for (kind, name) in [("diagram.svg", "test.svg"), ("diagram.png", "test.png")] {
            app.panel_submit(kind, name)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.studio.job.is_some() && Instant::now() < deadline {
                app.poll_studio();
                thread::sleep(Duration::from_millis(10));
            }
            let bytes = fs::read(root.path().join(name))?;
            if name.ends_with("svg") {
                assert!(String::from_utf8(bytes.clone())?.contains("Привет"));
            } else {
                assert_eq!(&bytes[1..4], b"PNG");
            }
            app.panel_submit(kind, name)?;
            while app.studio.job.is_some() && Instant::now() < deadline {
                app.poll_studio();
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(fs::read(root.path().join(name))?, bytes);
            assert!(app.status_error);
        }
        Ok(())
    }
    #[test]
    fn replacement_review_is_atomic_for_buffers_and_undoable() -> Result<()> {
        let root = tempfile::tempdir()?;
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        fs::write(&a, "one")?;
        fs::write(&b, "one")?;
        let mut app = App::new(root.path().into(), Language::Ru, vec![])?;
        app.open(&a)?;
        app.open(&b)?;
        app.studio.project_replace = true;
        app.studio.needle = "one".into();
        app.panel_submit("replace.with", "два")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.studio.job.is_some() && Instant::now() < deadline {
            app.poll_studio();
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.studio.changes.len(), 2);
        app.doc_mut().insert("changed");
        assert!(app.apply_review().is_err());
        assert_eq!(fs::read_to_string(&a)?, "one");
        app.doc_mut().undo();
        app.apply_review()?;
        assert!(app.documents.iter().all(|d| d.text == "два"));
        assert_eq!(fs::read_to_string(b)?, "one");
        Ok(())
    }
}
