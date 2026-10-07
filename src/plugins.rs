use crate::process;
use anyhow::{Context, Result, bail};
use mlua::{HookTriggers, Lua, LuaSerdeExt, VmState};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Deserialize)]
pub struct PluginCommand {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub titles: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub key: Option<String>,
}
#[derive(Clone, Deserialize)]
pub struct Manifest {
    #[serde(default = "api_version")]
    pub api_version: u32,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub entry: PathBuf,
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub settings: serde_json::Value,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
fn api_version() -> u32 {
    1
}
pub struct Plugin {
    pub manifest: Manifest,
    pub entry: PathBuf,
    pub manifest_path: PathBuf,
}
#[derive(Default)]
pub struct PluginManager {
    pub plugins: Vec<Plugin>,
}
#[derive(Serialize)]
pub struct PluginInput<'a> {
    pub event: &'a str,
    pub text: &'a str,
    pub path: Option<String>,
    pub language: &'a str,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPanel {
    pub title: String,
    pub content: String,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginOutput {
    pub action: Option<HostAction>,
    pub text: Option<String>,
    pub message: Option<String>,
    pub panel: Option<PluginPanel>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAction {
    pub kind: String,
    pub command: String,
}

impl PluginManager {
    /// Load ordered plugin directories; the first manifest with a given name wins.
    pub fn load(dirs: &[PathBuf]) -> (Self, Vec<String>) {
        let mut manager = Self::default();
        let mut errors = vec![];
        let mut names = HashSet::new();
        for dir in dirs {
            let entries = match fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) => {
                    errors.push(format!("{}: {e}", dir.display()));
                    continue;
                }
            };
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path().join("plugin.toml")))
                .filter(|p| p.is_file())
                .collect();
            if dir.join("plugin.toml").is_file() {
                paths.push(dir.join("plugin.toml"));
            }
            paths.sort();
            for manifest_path in paths {
                let loaded = (|| -> Result<Option<Plugin>> {
                    let manifest: Manifest = toml::from_str(&fs::read_to_string(&manifest_path)?)?;
                    if manifest.api_version != 1 {
                        bail!(
                            "Unsupported plugin API version {} (expected 1)",
                            manifest.api_version
                        );
                    }
                    let root = manifest_path.parent().unwrap().canonicalize()?;
                    let entry = root.join(&manifest.entry).canonicalize()?;
                    if !entry.starts_with(&root) {
                        bail!("Entry must be inside the plugin directory");
                    }
                    if !matches!(
                        entry.extension().and_then(|s| s.to_str()),
                        Some("js" | "ts" | "lua")
                    ) {
                        bail!("Expected .js, .ts or .lua entry");
                    }
                    if manifest.icon.as_ref().is_some_and(|icon| {
                        icon.is_empty() || icon.width() > 2 || icon.chars().any(char::is_control)
                    }) {
                        bail!(
                            "Plugin icon must occupy at most two columns and contain no controls"
                        );
                    }
                    if manifest.name.trim().is_empty()
                        || manifest
                            .commands
                            .iter()
                            .any(|c| c.id.is_empty() || c.title.is_empty())
                    {
                        bail!("Empty plugin name or command");
                    }
                    let mut ids = HashSet::new();
                    if manifest.commands.iter().any(|c| !ids.insert(&c.id)) {
                        bail!("Duplicate command id");
                    }
                    if manifest
                        .hooks
                        .iter()
                        .any(|h| !matches!(h.as_str(), "before_save" | "on_open"))
                    {
                        bail!("Unknown hook");
                    }
                    if !names.insert(manifest.name.clone()) {
                        return Ok(None);
                    }
                    Ok(Some(Plugin {
                        manifest,
                        entry,
                        manifest_path: manifest_path.clone(),
                    }))
                })();
                match loaded {
                    Ok(Some(p)) => manager.plugins.push(p),
                    Ok(None) => {}
                    Err(e) => errors.push(format!("{}: {e:#}", manifest_path.display())),
                }
            }
        }
        (manager, errors)
    }
    pub fn commands(&self) -> Vec<(usize, String, String)> {
        self.commands_for("en")
    }
    pub fn commands_for(&self, language: &str) -> Vec<(usize, String, String)> {
        self.plugins
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                p.manifest.commands.iter().map(move |c| {
                    (
                        i,
                        c.id.clone(),
                        format!(
                            "{} / {}",
                            p.manifest.name,
                            c.titles.get(language).unwrap_or(&c.title)
                        ),
                    )
                })
            })
            .collect()
    }
    pub fn execute(&self, index: usize, input: &PluginInput<'_>) -> Result<PluginOutput> {
        self.execute_with_settings(index, input, &serde_json::json!({}))
    }
    pub fn execute_with_settings(
        &self,
        index: usize,
        input: &PluginInput<'_>,
        settings: &serde_json::Value,
    ) -> Result<PluginOutput> {
        let plugin = self.plugins.get(index).context("Plugin not found")?;
        let mut payload = serde_json::to_value(input)?;
        payload["apiVersion"] = serde_json::json!(1);
        payload["settings"] = if plugin.manifest.settings.is_null() {
            serde_json::json!({})
        } else {
            plugin.manifest.settings.clone()
        };
        merge_json(&mut payload["settings"], settings.clone());
        let output = if plugin.entry.extension().and_then(|s| s.to_str()) == Some("lua") {
            run_lua(&plugin.entry, &payload)
        } else {
            run_js(&plugin.entry, &payload)
        };
        output.with_context(|| format!("Plugin {}", plugin.manifest.name))
    }
}

fn merge_json(base: &mut serde_json::Value, overlay: serde_json::Value) {
    if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in overlay {
            if let Some(old) = base.get_mut(key) {
                merge_json(old, value.clone());
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    } else if !overlay.is_null() {
        *base = overlay;
    }
}

fn run_lua(entry: &Path, input: &impl Serialize) -> Result<PluginOutput> {
    let lua = Lua::new();
    lua.set_memory_limit(32 * 1024 * 1024)?;
    // Plugins receive only the document API; remove filesystem/process/module access.
    for name in [
        "os", "io", "package", "debug", "dofile", "loadfile", "require", "print",
    ] {
        lua.globals().set(name, mlua::Value::Nil)?;
    }
    let start = Instant::now();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1000),
        move |_, _| {
            if start.elapsed() > Duration::from_millis(500) {
                Err(mlua::Error::RuntimeError("Plugin timed out".into()))
            } else {
                Ok(VmState::Continue)
            }
        },
    )?;
    let handler: mlua::Function = lua
        .load(fs::read_to_string(entry)?)
        .set_name(entry.to_string_lossy())
        .eval()?;
    let value: mlua::Value = handler.call(lua.to_value(input)?)?;
    if matches!(value, mlua::Value::Nil) {
        return Ok(PluginOutput::default());
    }
    Ok(lua.from_value(value)?)
}

fn run_js(entry: &Path, input: &impl Serialize) -> Result<PluginOutput> {
    let mut command = Command::new("node");
    command.args([
        "--disable-warning=ExperimentalWarning",
        "--max-old-space-size=64",
        "--input-type=module",
        "-e",
        include_str!("../runtime/runner.mjs"),
    ]);
    let payload = serde_json::json!({ "entry": entry, "input": input });
    let (success, stdout, stderr) =
        process::run(command, payload.to_string(), Duration::from_secs(3))
            .context("JS/TS plugins require Node.js >= 22.13")?;
    if !success {
        bail!("{}", stderr.trim());
    }
    serde_json::from_str(&stdout).context("Invalid plugin response")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn examples_all_runtimes() -> Result<()> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
        let (manager, errors) = PluginManager::load(&[root]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(manager.plugins.len(), 3);
        for (index, plugin) in manager.plugins.iter().enumerate() {
            let event = &plugin.manifest.commands[0].id;
            let output = manager.execute(
                index,
                &PluginInput {
                    event,
                    text: "hello   \n",
                    path: None,
                    language: "ru",
                },
            )?;
            assert!(output.text.is_some());
        }
        Ok(())
    }
    #[test]
    fn lua_timeout_and_removed_io() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("loop.lua");
        let input = PluginInput {
            event: "test",
            text: "",
            path: None,
            language: "en",
        };
        fs::write(&path, "return function(ctx) while true do end end")?;
        assert!(run_lua(&path, &input).is_err());
        fs::write(
            &path,
            "return function(ctx) return {message=tostring(io) .. tostring(os)} end",
        )?;
        assert_eq!(run_lua(&path, &input)?.message.as_deref(), Some("nilnil"));
        Ok(())
    }
    #[test]
    fn js_errors_and_timeouts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("broken.js");
        let input = PluginInput {
            event: "test",
            text: "",
            path: None,
            language: "en",
        };
        fs::write(
            &path,
            "module.exports = ctx => { throw new Error('broken'); }",
        )?;
        assert!(
            run_js(&path, &input)
                .unwrap_err()
                .to_string()
                .contains("broken")
        );
        fs::write(&path, "module.exports = ctx => { while (true) {} }")?;
        assert!(run_js(&path, &input).is_err());
        Ok(())
    }
}
