use crate::app::Config;
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn user_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("REDITOR_USER_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("Cannot find user home; set REDITOR_USER_DIR")?;
    Ok(PathBuf::from(home).join(".reditor"))
}
pub fn recent_projects() -> Result<Vec<PathBuf>> {
    recent_projects_from(&user_dir()?)
}
fn recent_projects_from(user: &Path) -> Result<Vec<PathBuf>> {
    let path = user.join("projects.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let paths: Vec<PathBuf> = serde_json::from_slice(&fs::read(&path)?)?;
    Ok(paths.into_iter().filter(|path| path.is_dir()).collect())
}
pub fn register_project(path: &Path) -> Result<()> {
    register_project_in(&user_dir()?, path)
}
fn register_project_in(user: &Path, path: &Path) -> Result<()> {
    let path = path.canonicalize()?;
    if !path.is_dir() {
        bail!("Project is not a directory: {}", path.display());
    }
    fs::create_dir_all(user)?;
    let mut paths = recent_projects_from(user)?;
    paths.retain(|item| item != &path);
    paths.insert(0, path);
    paths.truncate(20);
    let mut temporary = tempfile::NamedTempFile::new_in(user)?;
    use std::io::Write;
    temporary.write_all(&serde_json::to_vec_pretty(&paths)?)?;
    temporary.persist(user.join("projects.json"))?;
    Ok(())
}
pub fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(old) = base.get_mut(&key) {
                    merge(old, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}
pub fn read(path: &Path) -> Result<toml::Value> {
    if !path.exists() {
        return Ok(toml::Value::Table(Default::default()));
    }
    toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("Invalid configuration: {}", path.display()))
}
pub fn load_from(user: &Path, root: &Path) -> Result<Config> {
    let mut value = read(&user.join("config.toml"))?;
    merge(&mut value, read(&root.join(".reditor/config.toml"))?);
    let config: Config = value.try_into()?;
    if !(1..=16).contains(&config.indent) {
        bail!("Indent must be 1..16");
    }
    Ok(config)
}
pub fn load(root: &Path) -> Result<Config> {
    load_from(&user_dir()?, root)
}
pub fn plugin_dirs(root: &Path, explicit: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut dirs = explicit.to_vec();
    // First match wins: CLI > project > user; project copies can override user plugins.
    for path in [root.join(".reditor/plugins"), user_dir()?.join("plugins")] {
        if path.is_dir() && !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    Ok(dirs)
}
pub fn plugin_settings(root: &Path, name: &str) -> Result<serde_json::Value> {
    let config = load(root)?;
    Ok(config
        .plugin_settings
        .get(name)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({})))
}
fn differences(value: &toml::Value, inherited: &toml::Value) -> Option<toml::Value> {
    if value == inherited {
        return None;
    }
    if let (Some(value), Some(inherited)) = (value.as_table(), inherited.as_table()) {
        let mut table = toml::map::Map::new();
        for (key, item) in value {
            if let Some(old) = inherited.get(key) {
                if let Some(changed) = differences(item, old) {
                    table.insert(key.clone(), changed);
                }
            } else {
                table.insert(key.clone(), item.clone());
            }
        }
        Some(toml::Value::Table(table))
    } else {
        Some(value.clone())
    }
}
pub fn project_config(config: &Config) -> Result<String> {
    let inherited: Config = read(&user_dir()?.join("config.toml"))?.try_into()?;
    let value = toml::Value::try_from(config)?;
    let base = toml::Value::try_from(inherited)?;
    let overlay = differences(&value, &base).unwrap_or(toml::Value::Table(Default::default()));
    Ok(toml::to_string_pretty(&overlay)?)
}
pub fn install(dir: &Path, name: &str) -> Result<PathBuf> {
    let (manifest, source) = match name {
        "php" => (
            include_str!("../bundled/php/plugin.toml"),
            include_str!("../bundled/php/main.lua"),
        ),
        "remote" => (
            include_str!("../bundled/remote/plugin.toml"),
            include_str!("../bundled/remote/main.lua"),
        ),
        "git" => (
            include_str!("../bundled/git/plugin.toml"),
            include_str!("../bundled/git/main.lua"),
        ),
        "formatter" => (
            include_str!("../bundled/formatter/plugin.toml"),
            include_str!("../bundled/formatter/main.lua"),
        ),
        "proofreader" => (
            include_str!("../bundled/proofreader/plugin.toml"),
            include_str!("../bundled/proofreader/main.lua"),
        ),
        _ => bail!(
            "Unknown bundled plugin: {name}; expected php, remote, git, formatter or proofreader"
        ),
    };
    let target = dir.join("plugins").join(name);
    if target.exists() {
        bail!("Plugin already exists: {}", target.display());
    }
    let parent = target.parent().context("Invalid plugin path")?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::tempdir_in(parent)?;
    fs::write(temporary.path().join("plugin.toml"), manifest)?;
    fs::write(temporary.path().join("main.lua"), source)?;
    fs::rename(temporary.path(), &target)?;
    let config = dir.join("config.toml");
    if !config.exists() {
        fs::write(
            config,
            "# reditor settings; project values override user values.\n# language = 'ru'\n# [plugin_settings.php]\n# php_binary = 'php'\n# port = 8080\n# [plugin_settings.git]\n# log_limit = 50\n",
        )?;
    }
    if name == "remote" && !dir.join("connections.toml").exists() {
        fs::write(
            dir.join("connections.toml"),
            include_str!("../bundled/remote/connections.example.toml"),
        )?;
    }
    Ok(target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_projects_are_unique_and_ordered() -> Result<()> {
        let user = tempfile::tempdir()?;
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        register_project_in(user.path(), first.path())?;
        register_project_in(user.path(), second.path())?;
        register_project_in(user.path(), first.path())?;
        assert_eq!(
            recent_projects_from(user.path())?,
            vec![first.path().canonicalize()?, second.path().canonicalize()?]
        );
        Ok(())
    }
    #[test]
    fn project_overrides_only_supplied_settings() -> Result<()> {
        let user = tempfile::tempdir()?;
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join(".reditor"))?;
        fs::write(
            user.path().join("config.toml"),
            "language='de'\nindent=2\n[keybindings]\n'Alt+x'='files'\n[plugin_settings.php]\nport=9000\nphp_binary='custom-php'\n",
        )?;
        fs::write(
            root.path().join(".reditor/config.toml"),
            "wrap=true\n[plugin_settings.php]\nport=8000\n",
        )?;
        let config = load_from(user.path(), root.path())?;
        assert_eq!(config.language, crate::i18n::Language::De);
        assert_eq!(config.indent, 2);
        assert!(config.wrap);
        assert_eq!(config.plugin_settings["php"]["port"], 8000);
        assert_eq!(config.plugin_settings["php"]["php_binary"], "custom-php");
        assert_eq!(config.keybindings["Alt+x"], "files");
        Ok(())
    }
}
