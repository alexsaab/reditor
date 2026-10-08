use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind,
    },
    execute,
};
use reditor::{app::App, i18n::Language, ui::Renderer};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "reditor",
    version,
    about = "Rust terminal editor · Rust-Terminaleditor · Editor de terminal · Терминальный редактор"
)]
struct Cli {
    /// Files to open, or a directory to browse
    paths: Vec<PathBuf>,
    /// Workspace directory (defaults to the first file's parent or current directory)
    #[arg(short, long)]
    workspace: Option<PathBuf>,
    /// Interface language / Язык интерфейса: ru, en, de, es
    #[arg(long, value_enum)]
    lang: Option<Language>,
    /// Explicitly load trusted plugin directories; may be repeated
    #[arg(long = "plugins")]
    plugins: Vec<PathBuf>,
    /// Install a bundled Lua plugin (php, remote, git, formatter, proofreader), then exit
    #[arg(long = "install-plugin")]
    install_plugins: Vec<String>,
    /// Installation scope: project or user (~/.reditor)
    #[arg(long, default_value = "project", value_parser = ["project", "user"])]
    scope: String,
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        ratatui::restore();
    }
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let root = if let Some(workspace) = cli.workspace {
        workspace
    } else if let Some(path) = cli.paths.first() {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        };
        if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(&cwd).to_owned()
        }
    } else {
        reditor::settings::recent_projects()?
            .into_iter()
            .next()
            .unwrap_or(cwd.clone())
    };
    if !cli.install_plugins.is_empty() {
        let directory = if cli.scope == "user" {
            reditor::settings::user_dir()?
        } else {
            root.join(".reditor")
        };
        for name in cli.install_plugins {
            println!(
                "{}",
                reditor::settings::install(&directory, &name)?.display()
            );
        }
        return Ok(());
    }
    let config = reditor::settings::load(&root)?;
    let mut app = App::new(root, cli.lang.unwrap_or(config.language), cli.plugins)?;
    let _ = reditor::settings::register_project(&app.root);
    let (documents, active) = reditor::session::load(&app.root)?;
    if !documents.is_empty() {
        app.documents = documents;
        app.active = active;
    }
    for path in cli.paths {
        if !path.is_dir() {
            app.open(&path)?;
        }
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!("reditor requires an interactive terminal / Нужен интерактивный терминал");
    }
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    let mut renderer = Renderer::new();
    renderer.query_graphics();
    while !app.quit {
        app.poll_build();
        app.poll_diagram();
        app.poll_studio();
        terminal.draw(|frame| renderer.draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    renderer.follow_cursor();
                    app.key(key);
                }
                Event::Paste(text) => {
                    renderer.follow_cursor();
                    app.paste(&text);
                }
                Event::Mouse(mouse) => renderer.mouse(&mut app, mouse),
                _ => {}
            }
        }
    }
    reditor::session::save(&app.root, &app.documents, app.active)?;
    Ok(())
}
