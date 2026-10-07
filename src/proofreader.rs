use crate::{
    app::App,
    process, settings,
    studio::{Action, Choice, JobResult},
};
use anyhow::{Context, Result, bail, ensure};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const MAX_TEXT: usize = 128 * 1024;
pub(crate) type ProofreadResult = Result<(u64, u64, CheckReport)>;

pub(crate) struct CheckReport {
    pub(crate) language: Option<String>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IssueKind {
    Spelling,
    Grammar,
}

#[derive(Clone, Debug)]
pub(crate) struct Diagnostic {
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) end_line: usize,
    pub(crate) end_column: usize,
    pub(crate) kind: IssueKind,
    pub(crate) message: String,
    pub(crate) replacements: Vec<String>,
}

pub(crate) struct ProofreadState {
    pub(crate) id: u64,
    pub(crate) revision: u64,
    pub(crate) language: Option<String>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

#[derive(Deserialize)]
struct Response {
    language: Option<ResponseLanguage>,
    matches: Vec<Match>,
}

#[derive(Deserialize)]
struct ResponseLanguage {
    code: String,
}

#[derive(Deserialize)]
struct Match {
    offset: usize,
    length: usize,
    message: String,
    #[serde(default)]
    replacements: Vec<Replacement>,
    rule: Option<Rule>,
}

#[derive(Deserialize)]
struct Rule {
    #[serde(rename = "issueType")]
    issue_type: Option<String>,
}

#[derive(Deserialize)]
struct Replacement {
    value: String,
}

struct MappedText {
    text: String,
    source_units: Vec<usize>,
}

impl MappedText {
    fn append(&mut self, source: &str, start: usize, end: usize, byte_units: &[usize]) {
        for (relative, ch) in source[start..end].char_indices() {
            let source_unit = byte_units[start + relative];
            self.text.push(ch);
            for offset in 0..ch.len_utf16() {
                self.source_units.push(source_unit + offset);
            }
        }
    }

    fn separator(&mut self, ch: char, source_unit: usize) {
        self.text.push(ch);
        self.source_units.push(source_unit);
    }

    fn new(source: &str, markdown: bool) -> Self {
        let mut byte_units = vec![0; source.len() + 1];
        let mut units = 0;
        for (byte, ch) in source.char_indices() {
            for slot in &mut byte_units[byte..byte + ch.len_utf8()] {
                *slot = units;
            }
            units += ch.len_utf16();
        }
        byte_units[source.len()] = units;
        let mut mapped = Self {
            text: String::new(),
            source_units: Vec::new(),
        };
        if !markdown {
            mapped.append(source, 0, source.len(), &byte_units);
        } else {
            let mut previous_end = 0;
            let mut code_block = 0usize;
            let mut image = 0usize;
            let options =
                Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
            for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
                match event {
                    Event::Start(Tag::CodeBlock(_)) => code_block += 1,
                    Event::End(TagEnd::CodeBlock) => code_block = code_block.saturating_sub(1),
                    Event::Start(Tag::Image { .. }) => image += 1,
                    Event::End(TagEnd::Image) => image = image.saturating_sub(1),
                    Event::Text(_)
                        if code_block == 0 && image == 0 && range.start >= previous_end =>
                    {
                        let gap = &source[previous_end..range.start];
                        let mut newlines = gap.match_indices('\n');
                        if let Some((offset, _)) = newlines.next() {
                            mapped.separator('\n', byte_units[previous_end + offset]);
                            if let Some((offset, _)) = newlines.next() {
                                mapped.separator('\n', byte_units[previous_end + offset]);
                            }
                        } else if gap.chars().any(char::is_alphanumeric)
                            && mapped
                                .text
                                .chars()
                                .last()
                                .is_some_and(|ch| !ch.is_whitespace())
                            && source[range.clone()]
                                .chars()
                                .next()
                                .is_some_and(|ch| !ch.is_whitespace())
                        {
                            mapped.separator(' ', byte_units[previous_end]);
                        }
                        mapped.append(source, range.start, range.end, &byte_units);
                        previous_end = range.end;
                    }
                    _ => {}
                }
            }
        }
        mapped.source_units.push(units);
        mapped
    }
}

fn positions(source: &str) -> Vec<(usize, usize)> {
    let mut positions = Vec::with_capacity(source.encode_utf16().count() + 1);
    let (mut line, mut column) = (0, 0);
    for ch in source.chars() {
        for _ in 0..ch.len_utf16() {
            positions.push((line, column));
        }
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    positions.push((line, column));
    positions
}

fn language(code: &str) -> Result<&'static str> {
    match code {
        "ru" => Ok("ru-RU"),
        "en" => Ok("en-US"),
        "de" => Ok("de-DE"),
        "es" => Ok("es"),
        _ => bail!("Unsupported proofreader language: {code}"),
    }
}

fn supported_language(code: &str) -> Option<&'static str> {
    match code.split('-').next()? {
        "ru" => Some("ru"),
        "en" => Some("en"),
        "de" => Some("de"),
        "es" => Some("es"),
        _ => None,
    }
}

// A few distinctive words help with short texts, where LanguageTool may confuse
// Spanish with Catalan or lack enough context to identify the language.
fn short_text_language(text: &str) -> Option<&'static str> {
    if text.chars().count() > 240 {
        return None;
    }
    let mut scores = [0u8; 4];
    let mut seen = std::collections::HashSet::new();
    let lowered = text.to_lowercase();
    for word in lowered
        .split(|ch: char| !ch.is_alphabetic())
        .filter(|word| !word.is_empty())
    {
        if !seen.insert(word) {
            continue;
        }
        let (index, weight) = match word {
            "это" | "что" | "как" | "для" | "есть" | "привет" | "русский" | "ошибка" => {
                (0, 2)
            }
            "the" | "this" | "that" | "with" | "and" | "are" | "has" | "have" | "there"
            | "english" | "sentence" => (1, 2),
            "das" | "ist" | "eine" | "und" | "nicht" | "der" | "die" | "deutsch" | "satz" => (2, 2),
            "esta" | "este" | "para" | "pero" | "español" => (3, 3),
            "el" | "la" | "los" | "las" | "que" | "una" | "idioma" => (3, 1),
            _ => continue,
        };
        scores[index] = scores[index].saturating_add(weight);
    }
    let (best_index, &best) = scores.iter().enumerate().max_by_key(|(_, score)| *score)?;
    let second = scores
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != best_index)
        .map(|(_, score)| *score)
        .max()
        .unwrap_or(0);
    (best >= 4 && best >= second + 2).then(|| ["ru", "en", "de", "es"][best_index])
}

fn run_language_tool(java: &str, jar: &Path, text: &str, code: Option<&str>) -> Result<Response> {
    let mut command = Command::new(java);
    command.args([
        "-jar",
        &jar.to_string_lossy(),
        "--json",
        "--clean-overlapping",
        "-c",
        "utf-8",
    ]);
    if let Some(code) = code {
        command.args(["-l", language(code)?]);
    } else {
        command.arg("--autoDetect");
    }
    command.arg("-");
    command.current_dir(jar.parent().context("Invalid LanguageTool JAR path")?);
    let (success, out, err) = process::run(command, text.to_owned(), Duration::from_secs(60))
        .context("Java 17+ is required for local LanguageTool")?;
    ensure!(success, "LanguageTool: {}", err.trim());
    // With --autoDetect and no fastText model, LanguageTool may print a
    // language-detection warning to stdout before the JSON response.
    let json = out
        .find("{\"software\"")
        .or_else(|| out.find('{'))
        .map(|offset| &out[offset..])
        .context("LanguageTool returned no JSON response")?;
    serde_json::from_str(json).context("Invalid LanguageTool response")
}

fn jar(root: &Path) -> Result<PathBuf> {
    if let Some(value) = settings::load(root)?
        .plugin_settings
        .get("proofreader")
        .and_then(|settings| settings.get("jar"))
    {
        return value
            .as_str()
            .map(PathBuf::from)
            .context("proofreader.jar must be a path");
    }
    for path in [
        root.join(".reditor/tools/languagetool/languagetool-commandline.jar"),
        settings::user_dir()?.join("tools/languagetool/languagetool-commandline.jar"),
    ] {
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("LanguageTool is not installed; see docs/PROOFREADER.md")
}

pub(crate) fn check(root: &Path, source: &str, markdown: bool, code: &str) -> Result<CheckReport> {
    ensure!(
        source.len() <= MAX_TEXT,
        "Proofreader supports files up to 128 KiB"
    );
    if code != "auto" {
        language(code)?;
    }
    let mapped = MappedText::new(source, markdown);
    if mapped.text.trim().is_empty() {
        return Ok(CheckReport {
            language: None,
            diagnostics: Vec::new(),
        });
    }
    let jar = jar(root)?;
    ensure!(
        jar.is_file(),
        "LanguageTool JAR not found: {}",
        jar.display()
    );
    let java = settings::load(root)?
        .plugin_settings
        .get("proofreader")
        .and_then(|settings| settings.get("java_binary"))
        .and_then(|value| value.as_str())
        .unwrap_or("java")
        .to_owned();
    let (detected, response) = if code == "auto" {
        if let Some(hint) = short_text_language(&mapped.text) {
            (
                Some(hint),
                run_language_tool(&java, &jar, &mapped.text, Some(hint))?,
            )
        } else {
            let detected_response = run_language_tool(&java, &jar, &mapped.text, None)?;
            let detected = detected_response
                .language
                .as_ref()
                .and_then(|language| supported_language(&language.code));
            let Some(detected) = detected else {
                return Ok(CheckReport {
                    language: None,
                    diagnostics: Vec::new(),
                });
            };
            let full_code = language(detected)?;
            if detected_response
                .language
                .as_ref()
                .is_some_and(|found| found.code == full_code)
            {
                (Some(detected), detected_response)
            } else {
                (
                    Some(detected),
                    run_language_tool(&java, &jar, &mapped.text, Some(detected))?,
                )
            }
        }
    } else {
        (
            Some(code),
            run_language_tool(&java, &jar, &mapped.text, Some(code))?,
        )
    };
    let positions = positions(source);
    let mut diagnostics = Vec::new();
    for found in response.matches.into_iter().take(300) {
        if found.length == 0 {
            continue;
        }
        let Some(&source_unit) = mapped.source_units.get(found.offset) else {
            continue;
        };
        let Some(&(line, column)) = positions.get(source_unit) else {
            continue;
        };
        let end_unit = found
            .offset
            .checked_add(found.length)
            .and_then(|end| end.checked_sub(1))
            .and_then(|last| mapped.source_units.get(last))
            .map(|last| last + 1)
            .unwrap_or(source_unit);
        let &(end_line, end_column) = positions.get(end_unit).unwrap_or(&(line, column));
        diagnostics.push(Diagnostic {
            line,
            column,
            end_line,
            end_column,
            kind: if found.rule.and_then(|rule| rule.issue_type).as_deref() == Some("misspelling") {
                IssueKind::Spelling
            } else {
                IssueKind::Grammar
            },
            message: found.message.replace(['\n', '\r'], " "),
            replacements: found
                .replacements
                .into_iter()
                .take(3)
                .map(|replacement| replacement.value)
                .collect(),
        });
    }
    Ok(CheckReport {
        language: detected.map(str::to_owned),
        diagnostics,
    })
}

impl App {
    pub(crate) fn proofread_command(&mut self, command: &str) -> Result<()> {
        let code = if command == "current" {
            self.i18n.code().to_ascii_lowercase()
        } else {
            command.to_owned()
        };
        if code != "auto" {
            language(&code)?;
        }
        let path = self
            .doc()
            .path
            .clone()
            .context(self.i18n.t("proofreader_save_first").to_owned())?;
        let markdown = match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "txt" => false,
            "md" => true,
            _ => bail!("{}", self.i18n.t("proofreader_txt_md_only")),
        };
        let source = self.doc().text.to_string();
        let root = self.root.clone();
        let (id, revision) = (self.doc().id, self.doc().revision);
        self.job("proofreader", move || {
            Ok(JobResult::Proofread {
                id,
                revision,
                report: check(&root, &source, markdown, &code)?,
                show_panel: true,
            })
        })?;
        self.studio.proofread_job = None;
        self.studio.proofread_requested = Some((id, revision));
        Ok(())
    }

    pub(crate) fn proofread_result(
        &mut self,
        id: u64,
        revision: u64,
        report: CheckReport,
        show_panel: bool,
    ) -> Result<()> {
        let Some(document) = self.documents.iter().find(|document| document.id == id) else {
            if show_panel {
                bail!("Document was closed during proofreading");
            }
            return Ok(());
        };
        if document.revision != revision {
            if show_panel {
                bail!("Document changed during proofreading; result discarded");
            }
            return Ok(());
        }
        let path = document
            .path
            .clone()
            .context("Proofread file has no path")?;
        let CheckReport {
            language,
            diagnostics,
        } = report;
        self.studio.proofread = Some(ProofreadState {
            id,
            revision,
            language: language.clone(),
            diagnostics: diagnostics.clone(),
        });
        self.studio.proofread_generation = self.studio.proofread_generation.wrapping_add(1);
        if !show_panel {
            return Ok(());
        }
        if diagnostics.is_empty() {
            self.text_panel("proofreader_clean", String::new());
            if let Some(language) = language
                && let Some(panel) = self.studio.panel.as_mut()
            {
                panel
                    .title
                    .push_str(&format!(" · {}", language.to_uppercase()));
            }
            return Ok(());
        }
        let items = diagnostics
            .into_iter()
            .map(|diagnostic| {
                let message = diagnostic.message.chars().take(100).collect::<String>();
                let suggestion = diagnostic
                    .replacements
                    .first()
                    .map(|value| format!(" → {value}"))
                    .unwrap_or_default();
                Choice {
                    label: format!(
                        "{}:{}  {}{}",
                        diagnostic.line + 1,
                        diagnostic.column + 1,
                        message,
                        suggestion
                    ),
                    action: Action::Jump(path.clone(), diagnostic.line, diagnostic.column, false),
                }
            })
            .collect();
        self.select("proofreader_results", items);
        if let Some(language) = language
            && let Some(panel) = self.studio.panel.as_mut()
        {
            panel
                .title
                .push_str(&format!(" · {}", language.to_uppercase()));
        }
        Ok(())
    }

    pub(crate) fn proofread_auto(&mut self) {
        if !self
            .plugins
            .plugins
            .iter()
            .any(|plugin| plugin.manifest.name == "proofreader")
        {
            return;
        }
        let document = self.doc();
        let key = (document.id, document.revision);
        if self.studio.proofread_observed != Some(key) {
            self.studio.proofread_observed = Some(key);
            self.studio.proofread_changed = std::time::Instant::now();
            return;
        }
        if self.studio.proofread_changed.elapsed() < Duration::from_millis(1500)
            || self.studio.proofread_requested == Some(key)
            || self.studio.proofread_job.is_some()
        {
            return;
        }
        let Some(path) = document.path.as_ref() else {
            return;
        };
        let markdown = match path.extension().and_then(|extension| extension.to_str()) {
            Some("txt") => false,
            Some("md") => true,
            _ => return,
        };
        let source = document.text.to_string();
        if source.len() > MAX_TEXT || jar(&self.root).is_err() {
            return;
        }
        let code = self
            .studio
            .config
            .plugin_settings
            .get("proofreader")
            .and_then(|settings| settings.get("language"))
            .and_then(|value| value.as_str())
            .unwrap_or("auto")
            .to_ascii_lowercase();
        if code != "auto" && language(&code).is_err() {
            return;
        }
        let root = self.root.clone();
        let (id, revision) = key;
        self.studio.proofread_requested = Some(key);
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result =
                check(&root, &source, markdown, &code).map(|report| (id, revision, report));
            let _ = send.send(result);
        });
        self.studio.proofread_job = Some(receive);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_keeps_prose_and_source_positions() {
        let source = "# This **are** wrong.\n\n`mistakke` [sentense](https://example.test/typo)\n\n```rust\nfn mistakke() {}\n```\n";
        let mapped = MappedText::new(source, true);
        assert!(mapped.text.contains("This are wrong."));
        assert!(mapped.text.contains("sentense"));
        assert!(!mapped.text.contains("mistakke"));
        assert!(!mapped.text.contains("example.test"));
        let output_unit = mapped.text.find("sentense").unwrap();
        let source_unit = mapped.source_units[output_unit];
        assert_eq!(positions(source)[source_unit], (2, 12));
    }

    #[test]
    fn short_text_language_uses_prose_words_without_guessing_ambiguous_text() {
        assert_eq!(
            short_text_language("Это русский текст, и это важно."),
            Some("ru")
        );
        assert_eq!(
            short_text_language("The sentense has a mistakke."),
            Some("en")
        );
        assert_eq!(short_text_language("Das ist ist falsch."), Some("de"));
        assert_eq!(short_text_language("El arbol esta verde."), Some("es"));
        assert_eq!(short_text_language("El gat i la casa."), None);
        assert_eq!(short_text_language("mistakke"), None);
    }

    #[test]
    #[ignore = "requires locally installed LanguageTool 6.6 and Java 17+"]
    fn real_four_languages_and_markdown() -> Result<()> {
        let root = tempfile::tempdir()?;
        for (code, text) in [
            ("ru", "Это ошипка."),
            ("en", "The sentense has a mistakke."),
            ("de", "Das ist ist falsch."),
            ("es", "El arbol esta verde."),
        ] {
            let findings = check(root.path(), text, false, code)?;
            assert!(!findings.diagnostics.is_empty(), "{code}");
        }
        let findings = check(
            root.path(),
            "# The **sentense** is wrong.\n\n`mistakke`",
            true,
            "en",
        )?;
        assert!(findings.diagnostics.iter().any(|finding| finding.line == 0));
        assert!(!findings.diagnostics.iter().any(|finding| finding.line == 2));
        let findings = check(root.path(), "😀 The sentense is wrong.", false, "en")?;
        assert!(
            findings
                .diagnostics
                .iter()
                .any(|finding| finding.column == 6)
        );
        for (expected, text) in [
            ("ru", "Это простой русский текст. В нём есть ошипка."),
            ("en", "The sentense has a mistakke."),
            ("de", "Das ist ist falsch."),
            ("es", "El arbol esta verde."),
        ] {
            let findings = check(root.path(), text, false, "auto")?;
            assert_eq!(findings.language.as_deref(), Some(expected));
            assert!(!findings.diagnostics.is_empty(), "{expected}");
        }
        let findings = check(
            root.path(),
            "An errorr occurs in a sentence.",
            false,
            "auto",
        )?;
        assert_eq!(findings.language.as_deref(), Some("en"));
        assert!(!findings.diagnostics.is_empty());
        let findings = check(
            root.path(),
            "# The **sentense** has a mistakke.\n\n`Das ist ist falsch.`",
            true,
            "auto",
        )?;
        assert_eq!(findings.language.as_deref(), Some("en"));
        assert!(findings.diagnostics.iter().all(|finding| finding.line == 0));

        settings::install(&root.path().join(".reditor"), "proofreader")?;
        let path = root.path().join("notes.txt");
        std::fs::write(&path, "The sentense has a mistakke.")?;
        let mut app = App::new(root.path().into(), crate::i18n::Language::Ru, vec![])?;
        app.open(&path)?;
        app.studio.proofread_observed = Some((app.doc().id, app.doc().revision));
        app.studio.proofread_changed = std::time::Instant::now() - Duration::from_secs(2);
        app.poll_studio();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.studio.proofread_job.is_some() && std::time::Instant::now() < deadline {
            app.poll_studio();
            std::thread::sleep(Duration::from_millis(10));
        }
        let proofread = app
            .studio
            .proofread
            .as_ref()
            .context("No automatic results")?;
        assert_eq!(proofread.language.as_deref(), Some("en"));
        assert!(!proofread.diagnostics.is_empty());
        Ok(())
    }
}
