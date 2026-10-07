use crate::{app::App, process, settings, studio::JobResult};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const MAX_SOURCE: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    Rust,
    Php,
    Prettier,
}
fn engine(path: &Path) -> Result<Engine> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Ok(Engine::Rust),
        "php" | "phtml" => Ok(Engine::Php),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "html" | "htm" | "css"
        | "scss" | "less" | "json" | "jsonc" | "md" | "markdown" | "yaml" | "yml" | "vue"
        | "graphql" | "gql" => Ok(Engine::Prettier),
        _ => bail!("Unsupported format: {}", path.display()),
    }
}
fn tool_setting(root: &Path, key: &str) -> Result<Option<PathBuf>> {
    let config = settings::load(root)?;
    config
        .plugin_settings
        .get("formatter")
        .and_then(|settings| settings.get(key))
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .with_context(|| format!("{key} must be a path/string"))
        })
        .transpose()
}
fn executable(root: &Path, path: &Path, key: &str, name: &str) -> Result<PathBuf> {
    if let Some(path) = tool_setting(root, key)? {
        ensure!(!path.as_os_str().is_empty(), "{key} cannot be empty");
        return Ok(path);
    }
    if name == "prettier" {
        let local = if cfg!(windows) {
            "prettier.cmd"
        } else {
            "prettier"
        };
        for dir in path.parent().into_iter().flat_map(Path::ancestors) {
            if !dir.starts_with(root) {
                break;
            }
            let candidate = dir.join("node_modules/.bin").join(local);
            if candidate.is_file() {
                return Ok(candidate);
            }
            if dir == root {
                break;
            }
        }
        let installed = settings::user_dir()?
            .join("tools/prettier/node_modules/.bin")
            .join(local);
        if installed.is_file() {
            return Ok(installed);
        }
        let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runtime/formatter/node_modules/.bin")
            .join(local);
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    if name == "php-cs-fixer" {
        let local = root.join("vendor/bin/php-cs-fixer");
        if local.is_file() {
            return Ok(local);
        }
        let installed = settings::user_dir()?.join("tools/php-cs-fixer.phar");
        if installed.is_file() {
            return Ok(installed);
        }
    }
    Ok(PathBuf::from(name))
}
fn edition(path: &Path, root: &Path) -> String {
    for dir in path.parent().into_iter().flat_map(Path::ancestors) {
        if !dir.starts_with(root) {
            break;
        }
        if let Ok(source) = fs::read_to_string(dir.join("Cargo.toml"))
            && let Ok(value) = toml::from_str::<toml::Value>(&source)
        {
            if let Some(edition) = value
                .get("package")
                .and_then(|p| p.get("edition"))
                .and_then(toml::Value::as_str)
            {
                return edition.into();
            }
            if let Some(edition) = value
                .get("workspace")
                .and_then(|p| p.get("package"))
                .and_then(|p| p.get("edition"))
                .and_then(toml::Value::as_str)
            {
                return edition.into();
            }
        }
        if dir == root {
            break;
        }
    }
    "2024".into()
}
fn run(command: Command, input: &str, tool: &str) -> Result<String> {
    let (success, out, err) = process::run(command, input.into(), Duration::from_secs(15))
        .with_context(|| {
            format!("{tool} not available. Install or configure it in [plugin_settings.formatter].")
        })?;
    ensure!(
        success,
        "{}: {}",
        tool,
        if err.trim().is_empty() {
            out.trim()
        } else {
            err.trim()
        }
    );
    ensure!(
        out.len() < 2 * 1024 * 1024,
        "Formatter output exceeds 2 MiB"
    );
    Ok(out)
}
pub fn format(root: &Path, path: &Path, source: &str) -> Result<String> {
    ensure!(
        source.len() <= MAX_SOURCE,
        "Formatter supports files up to 1 MiB"
    );
    match engine(path)? {
        Engine::Rust => {
            let tool = executable(root, path, "rustfmt_binary", "rustfmt")?;
            let mut command = Command::new(tool);
            command.args(["--edition", &edition(path, root), "--emit", "stdout"]);
            command.current_dir(root);
            run(command, source, "rustfmt")
        }
        Engine::Prettier => {
            let tool = executable(root, path, "prettier_binary", "prettier")?;
            let mut command = Command::new(tool);
            command.args(["--stdin-filepath", &path.to_string_lossy()]);
            command.current_dir(root);
            run(command, source, "Prettier")
        }
        Engine::Php => {
            let tool = executable(root, path, "php_cs_fixer_binary", "php-cs-fixer")?;
            let dir = root.join(".reditor/formatter/tmp");
            fs::create_dir_all(&dir)?;
            let file = tempfile::Builder::new()
                .prefix("reditor-")
                .suffix(".php")
                .tempfile_in(&dir)?;
            fs::write(file.path(), source)?;
            let mut previous = source.to_owned();
            for _ in 0..3 {
                let mut command = if tool
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("phar"))
                {
                    let mut command = Command::new("php");
                    command.arg(&tool);
                    command
                } else {
                    Command::new(&tool)
                };
                command.args([
                    "fix",
                    "--using-cache=no",
                    "--path-mode=override",
                    "--no-interaction",
                    "--quiet",
                ]);
                if !root.join(".php-cs-fixer.php").exists()
                    && !root.join(".php-cs-fixer.dist.php").exists()
                {
                    command.arg("--rules=@PSR12");
                }
                command.arg(file.path()).current_dir(root);
                let (ok, out, err) = process::run(command, String::new(), Duration::from_secs(20))
                    .with_context(|| format!("PHP CS Fixer unavailable: {}", tool.display()))?;
                ensure!(
                    ok,
                    "PHP CS Fixer: {}",
                    if err.trim().is_empty() {
                        out.trim()
                    } else {
                        err.trim()
                    }
                );
                ensure!(
                    fs::metadata(file.path())?.len() <= (2 * MAX_SOURCE) as u64,
                    "Formatted PHP output too large"
                );
                let current =
                    fs::read_to_string(file.path()).context("Formatter output must be UTF-8")?;
                if current == previous {
                    return Ok(current);
                }
                previous = current;
            }
            Ok(previous)
        }
    }
}
fn preview(before: &str, after: &str) -> String {
    let a: Vec<_> = before.lines().collect();
    let b: Vec<_> = after.lines().collect();
    let mut body = String::new();
    let mut shown = 0;
    for index in 0..a.len().max(b.len()) {
        if a.get(index) == b.get(index) {
            continue;
        }
        body.push_str(&format!(
            "{}:\n- {}\n+ {}\n",
            index + 1,
            a.get(index).copied().unwrap_or(""),
            b.get(index).copied().unwrap_or("")
        ));
        shown += 1;
        if shown == 40 {
            body.push_str("…\n");
            break;
        }
    }
    if body.is_empty() && before != after {
        body.push_str("Final newline differs\n");
    }
    body
}
impl App {
    pub(crate) fn formatter_enabled(&self) -> bool {
        self.plugins
            .plugins
            .iter()
            .any(|plugin| plugin.manifest.name == "formatter")
            && self
                .studio
                .config
                .plugin_settings
                .get("formatter")
                .and_then(|v| v.get("on_save"))
                .and_then(|v| v.as_bool())
                == Some(true)
    }
    pub(crate) fn formatter_on_save(&self, path: &Path, source: &str) -> Result<String> {
        if !self.formatter_enabled() || engine(path).is_err() {
            return Ok(source.into());
        }
        format(&self.root, path, source)
    }
    pub(crate) fn formatter_command(&mut self, command: &str) -> Result<()> {
        match command {
            "on_save" => {
                let enabled = !self.formatter_enabled();
                let settings = self
                    .studio
                    .config
                    .plugin_settings
                    .entry("formatter".into())
                    .or_insert_with(|| serde_json::json!({}));
                if !settings.is_object() {
                    *settings = serde_json::json!({});
                }
                settings["on_save"] = enabled.into();
                crate::studio::save_config(&self.root, &self.studio.config)?;
                self.status = self
                    .i18n
                    .t(if enabled {
                        "formatter_on"
                    } else {
                        "formatter_off"
                    })
                    .into();
                self.status_error = false;
                Ok(())
            }
            "document" | "check" => {
                let path = self
                    .doc()
                    .path
                    .clone()
                    .context(self.i18n.t("formatter_save_first").to_owned())?;
                engine(&path)?;
                let original = self.doc().text.to_string();
                let root = self.root.clone();
                let (id, revision) = (self.doc().id, self.doc().revision);
                let check = command == "check";
                self.job("formatter", move || {
                    let after = format(&root, &path, &original)?;
                    Ok(JobResult::Formatted {
                        id,
                        revision,
                        before: original,
                        after,
                        check,
                    })
                })
            }
            _ => bail!("Unknown formatter command"),
        }
    }
    pub(crate) fn formatter_result(
        &mut self,
        id: u64,
        revision: u64,
        before: String,
        after: String,
        check: bool,
    ) -> Result<()> {
        let document = self
            .documents
            .iter_mut()
            .find(|document| document.id == id)
            .context("Document was closed during formatting")?;
        ensure!(
            document.revision == revision,
            "Document changed during formatting; result discarded"
        );
        if before == after {
            self.status = self.i18n.t("formatter_clean").into();
            self.dialog = None;
            self.status_error = false;
        } else if check {
            self.text_panel("formatter_diff", preview(&before, &after));
        } else {
            document.replace(&after);
            self.status = self.i18n.t("formatter_done").into();
            self.dialog = None;
            self.status_error = false;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    #[test]
    fn rust_editions_check_and_undo() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("src"))?;
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n",
        )?;
        let path = root.path().join("src/main.rs");
        fs::write(&path, "fn main(){println!(\"hi\");}\n")?;
        let manifest = fs::read_to_string(root.path().join("Cargo.toml"))?;
        let _: toml::Value = toml::from_str(&manifest)?;
        assert_eq!(edition(&path, root.path()), "2021");
        assert_eq!(
            format(root.path(), &path, "fn main(){println!(\"hi\");}\n")?,
            "fn main() {\n    println!(\"hi\");\n}\n"
        );
        assert!(format(root.path(), &path, "fn main( {").is_err());
        assert!(preview("a\n", "a").contains("Final newline"));
        assert!(engine(Path::new("src/main.rs"))? == Engine::Rust);
        assert!(engine(Path::new("index.html"))? == Engine::Prettier);
        assert!(engine(Path::new("index.php"))? == Engine::Php);
        assert!(engine(Path::new("report.txt")).is_err());
        Ok(())
    }
    #[test]
    fn format_on_save_and_manual_result_are_undoable() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("main.rs");
        fs::write(&path, "fn main(){println!(\"hi\");}\n")?;
        let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled/formatter");
        let mut app = App::new(root.path().into(), Language::En, vec![plugin])?;
        app.open(&path)?;
        let plugin = app
            .plugins
            .plugins
            .iter()
            .position(|plugin| plugin.manifest.name == "formatter")
            .unwrap();
        app.execute_plugin(plugin, "formatter.on_save")?;
        assert!(app.formatter_enabled());
        app.save(&path, false)?;
        assert_eq!(
            fs::read_to_string(&path)?,
            "fn main() {\n    println!(\"hi\");\n}\n"
        );
        let saved = fs::read_to_string(&path)?;
        app.doc_mut().replace("fn main( {");
        assert!(app.save(&path, false).is_err());
        assert_eq!(fs::read_to_string(&path)?, saved);
        assert_eq!(app.doc().text.to_string(), "fn main( {");

        let before = "fn main(){println!(\"again\");}\n";
        app.doc_mut().replace(before);
        let id = app.doc().id;
        let revision = app.doc().revision;
        let after = format(root.path(), &path, before)?;
        app.formatter_result(id, revision, before.into(), after.clone(), true)?;
        assert_eq!(app.doc().text.to_string(), before);
        app.formatter_result(id, revision, before.into(), after.clone(), false)?;
        assert_eq!(app.doc().text.to_string(), after);
        app.doc_mut().undo();
        assert_eq!(app.doc().text.to_string(), before);
        assert!(
            app.formatter_result(id, revision, before.into(), after, false)
                .is_err()
        );
        app.execute_plugin(plugin, "formatter.format")?;
        for _ in 0..100 {
            app.poll_studio();
            if !app.job_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.job_running());
        assert_eq!(
            app.doc().text.to_string(),
            format(root.path(), &path, before)?
        );
        Ok(())
    }
    #[test]
    #[ignore = "requires Prettier and PHP CS Fixer tools"]
    fn real_multilanguage_formatters() -> Result<()> {
        let root = tempfile::tempdir()?;
        for (name, source, expected) in [
            ("main.rs", "fn main(){println!(\"hi\");}", "fn main() {"),
            (
                "file.php",
                "<?php function f( ){echo 'hi';}",
                "function f()",
            ),
            ("file.js", "const x={a:1,b:2}", "const x = { a: 1, b: 2 }"),
            ("file.ts", "const x:number=1", "const x: number = 1"),
            ("index.html", "<main><h1>Hi</h1></main>", "<h1>Hi</h1>"),
        ] {
            let output = format(root.path(), &root.path().join(name), source)
                .with_context(|| name.to_owned())?;
            assert!(output.contains(expected), "{name}: {output}");
            assert_eq!(
                format(root.path(), &root.path().join(name), &output)?,
                output,
                "{name} not idempotent"
            );
            if name.ends_with("js") || name.ends_with("php") {
                assert!(format(root.path(), &root.path().join(name), "<? broken@@").is_err());
            }
        }
        Ok(())
    }
}
