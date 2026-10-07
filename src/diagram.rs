use crate::{document::Document, process};
use anyhow::{Context, Result, bail};
use image::DynamicImage;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use serde::Deserialize;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct DiagramBlock {
    pub source: String,
    pub start: usize,
    pub end: usize,
}
pub fn blocks(document: &Document) -> Vec<DiagramBlock> {
    let text = document.text.to_string();
    if document
        .path
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|s| s.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mmd" | "mermaid"))
    {
        return vec![DiagramBlock {
            end: text.len(),
            source: text,
            start: 0,
        }];
    }
    let mut result = vec![];
    let mut current: Option<DiagramBlock> = None;
    for (event, range) in Parser::new(&text).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info
                    .split_whitespace()
                    .next()
                    .is_some_and(|s| s.eq_ignore_ascii_case("mermaid")) =>
            {
                current = Some(DiagramBlock {
                    source: String::new(),
                    start: range.end,
                    end: range.end,
                });
            }
            Event::Text(content) if current.is_some() => {
                let block = current.as_mut().unwrap();
                if block.source.is_empty() {
                    block.start = range.start;
                }
                block.source.push_str(&content);
                block.end = range.end;
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(block) = current.take() {
                    result.push(block);
                }
            }
            _ => {}
        }
    }
    result
}

#[derive(Debug, Deserialize)]
pub struct Geometry {
    pub width: f64,
    pub height: f64,
    pub labels: Vec<Label>,
    pub paths: Vec<Vec<[f64; 2]>>,
}
#[derive(Debug, Deserialize)]
pub struct Label {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
struct Rendered {
    image: DynamicImage,
    geometry: Geometry,
    svg: String,
}
struct Job {
    receive: Receiver<Result<Rendered>>,
    cancel: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub struct DiagramView {
    pub id: u64,
    pub blocks: Vec<DiagramBlock>,
    pub selected: usize,
    pub image: Option<DynamicImage>,
    pub geometry: Option<Geometry>,
    pub svg: Option<String>,
    pub graphics: bool,
    pub error: Option<String>,
    pub zoom: u16,
    pub pan_x: u32,
    pub pan_y: u32,
    pub revision: u64,
    job: Option<Job>,
}
impl DiagramView {
    pub fn new(blocks: Vec<DiagramBlock>, selected: usize) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            blocks,
            selected,
            image: None,
            geometry: None,
            svg: None,
            graphics: false,
            error: None,
            zoom: 100,
            pan_x: 0,
            pan_y: 0,
            revision: 0,
            job: None,
        }
    }
    pub fn loading(&self) -> bool {
        self.job.is_some()
    }
    pub fn start(&mut self) {
        self.job = None;
        self.image = None;
        self.geometry = None;
        self.svg = None;
        self.error = None;
        self.pan_x = 0;
        self.pan_y = 0;
        self.zoom = 100;
        self.revision += 1;
        let source = self.blocks[self.selected].source.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let (send, receive) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = render(&source, token);
            let _ = send.send(result);
        });
        self.job = Some(Job {
            receive,
            cancel,
            worker: Some(worker),
        });
    }
    pub fn poll(&mut self) {
        if let Some(result) = self
            .job
            .as_ref()
            .and_then(|job| job.receive.try_recv().ok())
        {
            self.job = None;
            match result {
                Ok(rendered) => {
                    self.image = Some(rendered.image);
                    self.geometry = Some(rendered.geometry);
                    self.svg = Some(rendered.svg);
                }
                Err(error) => self.error = Some(format!("{error:#}")),
            }
            self.revision += 1;
        }
    }
}
fn render(source: &str, cancel: Arc<AtomicBool>) -> Result<Rendered> {
    let runtime = std::env::var_os("REDITOR_MERMAID_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime/mermaid"));
    let script = runtime.join("render.mjs");
    if !script.exists() {
        bail!("Mermaid runtime not found; set REDITOR_MERMAID_RUNTIME (see README.md)");
    }
    if source.len() > 100000 {
        bail!("Mermaid source exceeds 100 KB");
    }
    let output = tempfile::tempdir()?;
    let input = serde_json::json!({ "source": source, "output": output.path() }).to_string();
    let mut command = Command::new("node");
    command.arg(script).current_dir(&runtime);
    let (success, _, stderr) =
        process::run_cancellable(command, input, Duration::from_secs(30), cancel)
            .context("Mermaid requires Node.js, npm ci and Playwright Chromium (see README.md)")?;
    if !success {
        bail!("{}", stderr.trim());
    }
    let png = fs::read(output.path().join("diagram.png"))?;
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .context("Invalid Mermaid image")?;
    let geometry = serde_json::from_slice(&fs::read(output.path().join("diagram.json"))?)
        .context("Invalid Mermaid vector geometry")?;
    let svg = fs::read_to_string(output.path().join("diagram.svg"))?;
    Ok(Rendered {
        image,
        geometry,
        svg,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fenced_and_standalone_diagrams_follow_source_offsets() {
        let mut doc = Document::new();
        doc.insert("# 🦀\n\n```mermaid\nflowchart LR\n A --> B\n```\n\n~~~mermaid\nsequenceDiagram\n A->>B: Hi\n~~~\n```rust\nfn main() {}\n```\n");
        let found = blocks(&doc);
        assert_eq!(found.len(), 2);
        for block in &found {
            assert_eq!(&doc.text.to_string()[block.start..block.end], block.source);
        }
        doc.path = Some(PathBuf::from("diagram.mmd"));
        doc.replace("flowchart LR\n A --> B");
        assert_eq!(blocks(&doc)[0].source, doc.text.to_string());
    }
}
