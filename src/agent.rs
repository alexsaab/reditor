use crate::plugins::PluginManager;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AgentInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub binary: &'static str,
    pub path: Option<PathBuf>,
}

pub const KNOWN_AGENTS: &[(&str, &str, &str)] = &[
    ("agy", "Antigravity CLI (agy)", "agy"),
    ("claude", "Claude Code (claude)", "claude"),
    ("codex", "OpenAI Codex CLI (codex)", "codex"),
    ("aider", "Aider AI Pair Programmer (aider)", "aider"),
    ("gemini", "Gemini CLI (gemini)", "gemini"),
];

/// Checks whether a binary exists in the system PATH and is executable.
pub fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in &["exe", "cmd", "bat"] {
                let with_ext = candidate.with_extension(ext);
                if is_executable_file(&with_ext) {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = path.metadata() {
            return meta.permissions().mode() & 0o111 != 0;
        }
    }
    true
}

/// Detects all known AI agents in PATH.
pub fn detect_agents() -> Vec<AgentInfo> {
    KNOWN_AGENTS
        .iter()
        .map(|&(id, name, binary)| {
            let path = find_in_path(binary);
            AgentInfo {
                id,
                name,
                binary,
                path,
            }
        })
        .collect()
}

/// Runs full diagnostics for installed plugins and AI agents.
pub fn diagnostics(root: &Path, plugin_manager: &PluginManager) -> String {
    let mut out = String::new();
    out.push_str("=== Reditor: Diagnostics (Plugins & AI Agents) ===\n\n");

    out.push_str("● AI Agents (CLI):\n");
    let agents = detect_agents();
    let mut found_any_agent = false;
    for agent in &agents {
        if let Some(path) = &agent.path {
            found_any_agent = true;
            out.push_str(&format!("  ✓ {} -> {}\n", agent.name, path.display()));
        } else {
            out.push_str(&format!("  × {} -> not found in PATH\n", agent.name));
        }
    }
    if !found_any_agent {
        out.push_str("  ! Hint: Install `agy`, `claude`, `codex`, or `aider` in your PATH.\n");
    }
    out.push('\n');

    out.push_str("● Loaded Plugins:\n");
    if plugin_manager.plugins.is_empty() {
        out.push_str("  (none loaded)\n");
    } else {
        for plugin in &plugin_manager.plugins {
            let entry_exists = plugin.entry.is_file();
            let status = if entry_exists { "OK" } else { "MISSING ENTRY" };
            out.push_str(&format!(
                "  • {} (v{}) [{status}]\n    entry: {}\n    manifest: {}\n",
                plugin.manifest.name,
                plugin.manifest.api_version,
                plugin.entry.display(),
                plugin.manifest_path.display()
            ));

            // Capability checks
            for cap in &plugin.manifest.capabilities {
                match cap.as_str() {
                    "php" => {
                        let php_path = find_in_path("php");
                        let status = match php_path {
                            Some(p) => format!("✓ php binary: {}", p.display()),
                            None => "× php binary: not found in PATH".into(),
                        };
                        out.push_str(&format!("    capability [php]: {status}\n"));
                    }
                    "git" => {
                        let git_path = find_in_path("git");
                        let status = match git_path {
                            Some(p) => format!("✓ git binary: {}", p.display()),
                            None => "× git binary: not found in PATH".into(),
                        };
                        out.push_str(&format!("    capability [git]: {status}\n"));
                    }
                    _ => {}
                }
            }
        }
    }
    out.push('\n');

    out.push_str("● Plugin Directories:\n");
    let project_plugins = root.join(".reditor/plugins");
    out.push_str(&format!(
        "  • Project: {} [{}]\n",
        project_plugins.display(),
        if project_plugins.is_dir() {
            "active"
        } else {
            "not present"
        }
    ));

    if let Ok(user_dir) = crate::settings::user_dir() {
        let user_plugins = user_dir.join("plugins");
        out.push_str(&format!(
            "  • User:    {} [{}]\n",
            user_plugins.display(),
            if user_plugins.is_dir() {
                "active"
            } else {
                "not present"
            }
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_detection_runs_without_panic() {
        let agents = detect_agents();
        assert_eq!(agents.len(), KNOWN_AGENTS.len());
        assert!(agents.iter().any(|a| a.id == "agy"));
    }

    #[test]
    fn diagnostics_produces_readable_report() {
        let manager = PluginManager::default();
        let report = diagnostics(Path::new("."), &manager);
        assert!(report.contains("AI Agents"));
        assert!(report.contains("Loaded Plugins"));
    }
}
