use crate::{app::App, plugins::HostAction, studio::JobResult, terminal::TerminalSession};
use anyhow::{Context, Result, bail, ensure};
use std::{process::Command, time::Duration};
impl App {
    pub(crate) fn plugin_action(&mut self, index: usize, action: HostAction) -> Result<()> {
        let plugin = self
            .plugins
            .plugins
            .get(index)
            .context("Plugin not found")?;
        ensure!(
            plugin.manifest.capabilities.contains(&action.kind),
            "Plugin {} needs capability {}",
            plugin.manifest.name,
            action.kind
        );
        match action.kind.as_str() {
            "remote" => self.remote_command(&action.command),
            "format" => self.formatter_command(&action.command),
            "proofread" => self.proofread_command(&action.command),
            "git" => {
                ensure!(
                    [
                        "git.status",
                        "git.diff",
                        "git.stage",
                        "git.commit",
                        "git.log"
                    ]
                    .contains(&action.command.as_str()),
                    "Unsupported Git plugin action"
                );
                if action.command == "git.log" {
                    let overrides =
                        crate::settings::plugin_settings(&self.root, &plugin.manifest.name)?;
                    let limit = overrides
                        .get("log_limit")
                        .or_else(|| plugin.manifest.settings.get("log_limit"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(50)
                        .clamp(1, 500);
                    let root = self.root.clone();
                    self.job("git.log", move || {
                        Ok(JobResult::Text(crate::workspace::git(
                            &root,
                            &["log", "--oneline", "--decorate", "-n", &limit.to_string()],
                        )?))
                    })
                } else {
                    self.command(&action.command)
                }
            }
            "php" => {
                ensure!(
                    ["php.lint", "php.run", "php.server"].contains(&action.command.as_str()),
                    "Unsupported PHP plugin action"
                );
                let overrides =
                    crate::settings::plugin_settings(&self.root, &plugin.manifest.name)?;
                let binary = overrides
                    .get("php_binary")
                    .or_else(|| plugin.manifest.settings.get("php_binary"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("php")
                    .to_owned();
                ensure!(!binary.is_empty(), "php_binary cannot be empty");
                match action.command.as_str() {
                    "php.lint" => {
                        let text = self.doc().text.to_string();
                        let root = self.root.clone();
                        self.job("php.lint", move || {
                            let mut command = Command::new(binary);
                            command.arg("-l").current_dir(root);
                            let (_, out, err) =
                                crate::process::run(command, text, Duration::from_secs(15))
                                    .context("PHP CLI is required; configure php_binary")?;
                            Ok(JobResult::Text(format!("{out}{err}")))
                        })
                    }
                    "php.run" => {
                        let path = self
                            .doc()
                            .path
                            .as_ref()
                            .context("Save the PHP file before running")?;
                        ensure!(!self.doc().dirty(), "Save PHP changes before running");
                        ensure!(
                            path.extension()
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("php")),
                            "Expected a .php file"
                        );
                        let terminal = TerminalSession::spawn(
                            "PHP".into(),
                            &self.root,
                            &binary,
                            &[
                                "-f".into(),
                                path.to_string_lossy().into_owned(),
                                "--".into(),
                            ],
                        )?;
                        self.push_terminal(terminal);
                        Ok(())
                    }
                    "php.server" => {
                        let port = overrides
                            .get("port")
                            .or_else(|| plugin.manifest.settings.get("port"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(8080);
                        ensure!((1..=65535).contains(&port), "PHP port must be 1..65535");
                        let terminal = TerminalSession::spawn(
                            format!("PHP 127.0.0.1:{port}"),
                            &self.root,
                            &binary,
                            &[
                                "-S".into(),
                                format!("127.0.0.1:{port}"),
                                "-t".into(),
                                self.root.to_string_lossy().into_owned(),
                            ],
                        )?;
                        self.push_terminal(terminal);
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
            _ => bail!("Unknown host capability {}", action.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{i18n::Language, plugins::PluginInput};
    use std::{fs, path::PathBuf, time::Instant};
    #[test]
    fn bundled_lua_and_capabilities() -> Result<()> {
        let root = tempfile::tempdir()?;
        for name in ["php", "remote", "git"] {
            crate::settings::install(&root.path().join(".reditor"), name)?;
        }
        let mut app = App::new(root.path().into(), Language::Ru, vec![])?;
        let php = app
            .plugins
            .plugins
            .iter()
            .position(|p| p.manifest.name == "php")
            .unwrap();
        let remote = app
            .plugins
            .plugins
            .iter()
            .position(|p| p.manifest.name == "remote")
            .unwrap();
        let git = app
            .plugins
            .plugins
            .iter()
            .position(|p| p.manifest.name == "git")
            .unwrap();
        let output = app.plugins.execute(
            php,
            &PluginInput {
                event: "php.template",
                text: "",
                path: None,
                language: "ru",
            },
        )?;
        assert!(output.text.unwrap().contains("declare(strict_types=1)"));
        let output = app.plugins.execute(
            php,
            &PluginInput {
                event: "php.outline",
                text: "<?php\nclass Demo {\n public function hello() {}\n}\n",
                path: None,
                language: "ru",
            },
        )?;
        assert!(output.panel.unwrap().content.contains("3: function hello"));
        let output = app.plugins.execute(
            remote,
            &PluginInput {
                event: "remote.connect",
                text: "",
                path: None,
                language: "es",
            },
        )?;
        assert_eq!(output.action.unwrap().kind, "remote");
        let output = app.plugins.execute(
            git,
            &PluginInput {
                event: "git.log",
                text: "",
                path: None,
                language: "de",
            },
        )?;
        assert_eq!(output.action.unwrap().command, "git.log");
        assert!(
            app.plugins
                .commands_for("es")
                .iter()
                .any(|(_, _, label)| label.contains("Conectar al servidor"))
        );
        app.plugins.plugins[php].manifest.capabilities.clear();
        assert!(app.execute_plugin(php, "php.lint").is_err());
        app.plugins.plugins[php]
            .manifest
            .capabilities
            .push("php".into());
        assert!(
            app.plugin_action(
                php,
                HostAction {
                    kind: "php".into(),
                    command: "terminal.new".into()
                }
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn lua_git_history_uses_real_repository() -> Result<()> {
        let root = tempfile::tempdir()?;
        crate::workspace::git(root.path(), &["init", "-q"])?;
        crate::workspace::git(root.path(), &["config", "user.name", "Reditor Test"])?;
        crate::workspace::git(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        )?;
        fs::write(root.path().join("a.txt"), "hello")?;
        crate::workspace::git(root.path(), &["add", "a.txt"])?;
        crate::workspace::git(root.path(), &["commit", "-qm", "Lua Git history test"])?;
        let mut app = App::new(
            root.path().into(),
            Language::Ru,
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled/git")],
        )?;
        app.execute_plugin(0, "git.log")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.job_running() && Instant::now() < deadline {
            app.poll_studio();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.studio
                .panel
                .as_ref()
                .unwrap()
                .body
                .contains("Lua Git history test")
        );
        Ok(())
    }
    #[test]
    #[ignore = "requires PHP CLI and localhost access"]
    fn real_php_lua_lint_run_and_server() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut app = App::new(
            root.path().into(),
            Language::Ru,
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled/php")],
        )?;
        let lint = |app: &mut App| -> Result<()> {
            app.command("php.lint")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.job_running() && Instant::now() < deadline {
                app.poll_studio();
                std::thread::sleep(Duration::from_millis(10));
            }
            ensure!(!app.job_running(), "PHP lint timeout");
            Ok(())
        };
        app.doc_mut().replace("<?php function broken( {");
        lint(&mut app)?;
        assert!(
            app.studio
                .panel
                .as_ref()
                .unwrap()
                .body
                .to_lowercase()
                .contains("parse error")
        );
        app.doc_mut()
            .replace("<?php echo 'RUNPHP:' . trim(fgets(STDIN));");
        lint(&mut app)?;
        assert!(
            app.studio
                .panel
                .as_ref()
                .unwrap()
                .body
                .contains("No syntax errors")
        );
        app.dialog = None;
        app.save(&root.path().join("main.php"), false)?;
        app.command("php.run")?;
        app.paste("Привет 🦀\n");
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.studio.terminals[0].exit.is_none() && Instant::now() < deadline {
            app.poll_studio();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.studio.terminals[0].output.contains("RUNPHP:Привет 🦀"),
            "output={:?} exit={:?} dialog={} focus={}",
            app.studio.terminals[0].output,
            app.studio.terminals[0].exit,
            app.dialog.is_some(),
            app.studio.terminal_focus
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        fs::create_dir_all(root.path().join(".reditor"))?;
        fs::write(
            root.path().join(".reditor/config.toml"),
            format!("[plugin_settings.php]\nport={port}\n"),
        )?;
        fs::write(root.path().join("index.php"), "<?php echo 'SERVERPHP';")?;
        app.command("php.server")?;
        let mut response = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while response.is_empty() && Instant::now() < deadline {
            if let Ok(mut socket) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                use std::io::{Read, Write};
                socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                socket.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
                socket.read_to_string(&mut response)?;
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(response.contains("SERVERPHP"));
        Ok(())
    }
}
