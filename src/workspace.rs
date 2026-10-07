use crate::process;
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub fn files(root: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_str(),
                    Some("target" | "node_modules" | ".git" | ".reditor" | ".browsers")
                )
        })
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|entry| entry.into_path())
        .take(20000)
        .collect()
}
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub text: String,
}
pub fn search(root: &Path, needle: &str) -> Vec<Hit> {
    if needle.is_empty() {
        return vec![];
    }
    let mut hits = vec![];
    for path in files(root) {
        if fs::metadata(&path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if text.contains('\0') {
            continue;
        }
        for (line, content) in text.lines().enumerate() {
            for (byte, _) in content.match_indices(needle) {
                hits.push(Hit {
                    path: path.clone(),
                    line,
                    column: content[..byte].chars().count(),
                    text: content.to_owned(),
                });
                if hits.len() >= 5000 {
                    return hits;
                }
            }
        }
    }
    hits
}
pub fn cargo_root(root: &Path, file: Option<&Path>) -> Result<PathBuf> {
    for start in [file.and_then(Path::parent), Some(root)]
        .into_iter()
        .flatten()
    {
        for directory in start.ancestors() {
            if directory.join("Cargo.toml").exists() {
                return Ok(directory.into());
            }
        }
    }
    anyhow::bail!("Cargo.toml not found")
}
pub fn metadata(root: &Path) -> Result<Value> {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps"]);
    let (ok, out, err) = process::run(command, String::new(), Duration::from_secs(15))?;
    anyhow::ensure!(ok, "{err}");
    serde_json::from_str(&out).context("Invalid Cargo metadata")
}
pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command.current_dir(root).args(args);
    let (ok, out, err) = process::run(command, String::new(), Duration::from_secs(15))?;
    anyhow::ensure!(ok, "{err}");
    Ok(out)
}

pub type GitLines = std::collections::HashMap<PathBuf, std::collections::HashMap<usize, char>>;
pub fn git_diff(root: &Path, path: &Path) -> Result<String> {
    let path_string = path.to_str().context("UTF-8 Git path required")?;
    if git(root, &["ls-files", "--error-unmatch", "--", path_string]).is_err() {
        let contents = fs::read_to_string(path)?;
        anyhow::ensure!(contents.len() <= 2 * 1024 * 1024, "Diff exceeds 2 MiB");
        return Ok(format!(
            "+++ {}\n{}",
            path.display(),
            contents
                .lines()
                .map(|line| format!("+{line}\n"))
                .collect::<String>()
        ));
    }
    git(root, &["diff", "HEAD", "--", path_string]).or_else(|_| {
        Ok(format!(
            "{}{}",
            git(root, &["diff", "--cached", "--", path_string])?,
            git(root, &["diff", "--", path_string])?
        ))
    })
}
pub fn git_snapshot(root: &Path) -> Result<(String, GitLines)> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    let base = PathBuf::from(top.trim());
    let branch = git(root, &["branch", "--show-current"])?;
    // Untracked files receive an added marker without staging them.
    let diff = git(
        &base,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "HEAD",
            "--unified=0",
            "--",
        ],
    )
    .or_else(|_| {
        git(
            &base,
            &["-c", "core.quotePath=false", "diff", "--unified=0", "--"],
        )
    })?;
    let mut lines = GitLines::new();
    let mut current = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current = Some(base.join(path));
        } else if line.starts_with("@@ ")
            && let Some(path) = &current
            && let Some(range) = line
                .split_whitespace()
                .find_map(|part| part.strip_prefix('+'))
        {
            let mut values = range.split(',');
            let start = values
                .next()
                .unwrap_or("1")
                .parse::<usize>()
                .unwrap_or(1)
                .saturating_sub(1);
            let count = values.next().unwrap_or("1").parse::<usize>().unwrap_or(1);
            for row in start..start + count.clamp(1, 10000) {
                lines
                    .entry(path.clone())
                    .or_default()
                    .insert(row, if count == 0 { '−' } else { '│' });
            }
        }
    }
    Ok((branch.trim().to_owned(), lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_search_excludes_artifacts_and_uses_unicode_columns() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir_all(root.path().join("target"))?;
        fs::write(root.path().join("target/ignored.rs"), "needle")?;
        fs::write(root.path().join("test.rs"), "🦀 needle needle")?;
        let hits = search(root.path(), "needle");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].column, 2);
        Ok(())
    }
    #[test]
    fn git_snapshot_and_untracked_diff() -> Result<()> {
        let root = tempfile::tempdir()?;
        git(root.path(), &["init", "-q"])?;
        fs::write(root.path().join("tracked.txt"), "before\n")?;
        git(root.path(), &["add", "tracked.txt"])?;
        git(
            root.path(),
            &[
                "-c",
                "user.name=Reditor Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        )?;
        let path = root.path().join("tracked.txt");
        fs::write(&path, "after\n")?;
        let (branch, lines) = git_snapshot(root.path())?;
        assert!(!branch.is_empty());
        assert!(
            lines
                .get(&path.canonicalize()?)
                .is_some_and(|rows| rows.contains_key(&0))
        );
        assert!(git_diff(root.path(), &path)?.contains("+after"));
        let untracked = root.path().join("new.txt");
        fs::write(&untracked, "new 🦀\n")?;
        assert!(git_diff(root.path(), &untracked)?.contains("+new 🦀"));
        Ok(())
    }
}
