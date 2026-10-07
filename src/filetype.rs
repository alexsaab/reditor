use std::path::Path;
use syntect::parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet};

pub fn syntax_set() -> SyntaxSet {
    let syntaxes = two_face::syntax::extra_newlines();
    let needs_ron = syntaxes.find_syntax_by_extension("ron").is_none();
    let needs_mermaid = syntaxes.find_syntax_by_extension("mmd").is_none();
    if !needs_ron && !needs_mermaid {
        return syntaxes;
    }
    let mut builder = syntaxes.into_builder();
    if needs_ron {
        builder.add(
            SyntaxDefinition::load_from_str(
                include_str!("../syntaxes/RON.sublime-syntax"),
                true,
                None,
            )
            .expect("bundled RON syntax"),
        );
    }
    if needs_mermaid {
        builder.add(
            SyntaxDefinition::load_from_str(
                include_str!("../syntaxes/Mermaid.sublime-syntax"),
                true,
                None,
            )
            .expect("bundled Mermaid syntax"),
        );
    }
    builder.build()
}

/// Match both extensions and filenames; never read an additional copy from disk.
pub fn detect<'a>(
    syntaxes: &'a SyntaxSet,
    path: Option<&Path>,
    first_line: &str,
) -> &'a SyntaxReference {
    if let Some(path) = path {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let alias = match name {
            "Cargo.lock" => Some("toml"),
            "rust-toolchain" if first_line.trim_start().starts_with('[') => Some("toml"),
            "config"
                if path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|p| p == ".cargo") =>
            {
                Some("toml")
            }
            _ => match extension.as_str() {
                "mjs" | "cjs" => Some("js"),
                "mts" | "cts" => Some("ts"),
                "htm" => Some("html"),
                "markdown" | "mdown" => Some("md"),
                _ => None,
            },
        };
        if let Some(syntax) = alias.and_then(|e| syntaxes.find_syntax_by_extension(e)) {
            return syntax;
        }
        if let Some(syntax) = syntaxes
            .find_syntax_by_extension(&extension)
            .or_else(|| syntaxes.find_syntax_by_path(name))
        {
            return syntax;
        }
    }
    syntaxes
        .find_syntax_by_first_line(first_line)
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn web_and_rust_project_syntaxes() {
        let syntaxes = syntax_set();
        for path in [
            "README.md",
            "index.js",
            "index.mjs",
            "index.cjs",
            "app.ts",
            "app.tsx",
            "app.mts",
            "index.html",
            "style.css",
            "build.rs",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "rustfmt.toml",
            ".cargo/config",
            "settings.ron",
            "workflow.yaml",
            "data.json",
            "diagram.mmd",
            "diagram.mermaid",
        ] {
            assert_ne!(
                detect(&syntaxes, Some(Path::new(path)), "").name,
                "Plain Text",
                "{path}"
            );
        }
        assert_eq!(
            detect(&syntaxes, Some(Path::new("Cargo.lock")), "").name,
            detect(&syntaxes, Some(Path::new("Cargo.toml")), "").name
        );
        assert_eq!(
            detect(&syntaxes, Some(Path::new("notes.unknown")), "").name,
            "Plain Text"
        );
    }
}
