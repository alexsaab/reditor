use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind, MouseEventKind,
    },
    execute,
};
use reditor::{app::App, i18n::Language, ui::Renderer};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::{Duration, Instant},
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
    /// Install a bundled plugin (all, php, remote, git, formatter, proofreader, trim, header-js, uppercase-ts), then exit
    #[arg(long = "install-plugin")]
    install_plugins: Vec<String>,
    /// Install all bundled plugins and exit
    #[arg(long = "install-all-plugins")]
    install_all_plugins: bool,
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
    if cli.install_all_plugins || !cli.install_plugins.is_empty() {
        let directory = if cli.scope == "user" {
            reditor::settings::user_dir()?
        } else {
            root.join(".reditor")
        };
        if cli.install_all_plugins || cli.install_plugins.iter().any(|name| name == "all") {
            for path in reditor::settings::install_all(&directory)? {
                println!("{}", path.display());
            }
        } else {
            for name in cli.install_plugins {
                println!(
                    "{}",
                    reditor::settings::install(&directory, &name)?.display()
                );
            }
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
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        ratatui::restore();
        prev_hook(info);
    }));
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    let mut renderer = Renderer::new();
    renderer.query_graphics();
    let mut needs_draw = true;
    let mut last_draw = Instant::now();
    while !app.quit {
        if needs_draw || last_draw.elapsed() >= Duration::from_millis(50) {
            app.poll_build();
            app.poll_diagram();
            app.poll_studio();
            terminal.draw(|frame| renderer.draw(frame, &mut app))?;
            needs_draw = false;
            last_draw = Instant::now();
        }
        let timeout = Duration::from_millis(50).saturating_sub(last_draw.elapsed());
        if event::poll(timeout)? {
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        renderer.follow_cursor();
                        app.key(key);
                        needs_draw = true;
                        break;
                    }
                    Event::Paste(text) => {
                        renderer.follow_cursor();
                        app.paste(&text);
                        needs_draw = true;
                        break;
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::Moved => {}
                        MouseEventKind::Drag(_)
                        | MouseEventKind::ScrollUp
                        | MouseEventKind::ScrollDown
                        | MouseEventKind::ScrollLeft
                        | MouseEventKind::ScrollRight => {
                            renderer.mouse(&mut app, mouse);
                            needs_draw = true;
                        }
                        _ => {
                            renderer.mouse(&mut app, mouse);
                            needs_draw = true;
                            break;
                        }
                    },
                    Event::Resize(_, _) => {
                        needs_draw = true;
                        break;
                    }
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
    reditor::session::save(&app.root, &app.documents, app.active)?;
    Ok(())
}
