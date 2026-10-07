use crate::document::Document;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

pub struct Client {
    child: Child,
    input: ChildStdin,
    receive: Receiver<Result<Value>>,
    next: u64,
    ready: bool,
    pending: HashMap<u64, String>,
    synced: HashMap<String, (u64, u64)>,
    pub diagnostics: HashMap<String, Vec<Value>>,
    pub status: String,
    root: String,
    settings: Value,
}
pub enum Event {
    Response(String, Value),
    Error(String),
    Edit(Value),
}
pub fn uri(path: &Path) -> Result<String> {
    Ok(url::Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("Invalid file path"))?
        .into())
}
pub fn path(uri: &str) -> Result<PathBuf> {
    url::Url::parse(uri)?
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("Expected file URI"))
}
pub fn position(doc: &Document) -> Value {
    let (row, column) = doc.position();
    let units: usize = doc
        .line(row)
        .chars()
        .take(column)
        .map(char::len_utf16)
        .sum();
    json!({ "line": row, "character": units })
}
pub fn offset(text: &ropey::Rope, position: &Value) -> Result<usize> {
    let line = position["line"].as_u64().context("Missing LSP line")? as usize;
    let column = position["character"]
        .as_u64()
        .context("Missing LSP column")? as usize;
    if line >= text.len_lines() {
        bail!("LSP position outside document");
    }
    let mut units = 0;
    let mut chars = 0;
    for ch in text.line(line).chars() {
        if units >= column {
            break;
        }
        units += ch.len_utf16();
        chars += 1;
    }
    if units != column {
        bail!("LSP position splits a UTF-16 character");
    }
    Ok(text.line_to_char(line) + chars)
}
pub fn edits(text: &str, values: &[Value]) -> Result<String> {
    let mut rope = ropey::Rope::from_str(text);
    let mut changes = values
        .iter()
        .map(|edit| {
            Ok((
                offset(&rope, &edit["range"]["start"])?,
                offset(&rope, &edit["range"]["end"])?,
                edit["newText"]
                    .as_str()
                    .context("Missing replacement")?
                    .to_owned(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    changes.sort_by_key(|edit| edit.0);
    for pair in changes.windows(2) {
        anyhow::ensure!(pair[0].1 <= pair[1].0, "Overlapping edits");
    }
    for (start, end, replacement) in changes.into_iter().rev() {
        anyhow::ensure!(start <= end, "Reversed edit range");
        rope.remove(start..end);
        rope.insert(start, &replacement);
    }
    Ok(rope.to_string())
}
fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length.context("LSP Content-Length missing")?;
    anyhow::ensure!(length <= 16 * 1024 * 1024, "LSP message exceeds 16 MiB");
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}
impl Client {
    pub fn start(root: &Path, program: &str) -> Result<Self> {
        let mut child = Command::new(program)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("rust-analyzer: install with rustup component add rust-analyzer")?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (send, receive) = mpsc::sync_channel(64);
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                match read_message(&mut reader) {
                    Ok(Some(value)) => {
                        if send.send(Ok(value)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = send.send(Err(anyhow::anyhow!("rust-analyzer disconnected")));
                        break;
                    }
                    Err(e) => {
                        let _ = send.send(Err(e));
                        break;
                    }
                }
            }
        });
        let mut client = Self {
            child,
            input,
            receive,
            next: 1,
            ready: false,
            pending: HashMap::new(),
            synced: HashMap::new(),
            diagnostics: HashMap::new(),
            status: "initializing".into(),
            root: uri(root)?,
            settings: json!({ "checkOnSave": true, "files": { "watcher": "server" } }),
        };
        client.request("initialize", json!({ "processId": std::process::id(), "rootUri": uri(root)?, "capabilities": { "general": { "positionEncodings": ["utf-16"] }, "textDocument": { "completion": { "completionItem": { "snippetSupport": false, "resolveSupport": { "properties": ["additionalTextEdits"] } } }, "hover": { "contentFormat": ["plaintext"] }, "publishDiagnostics": { "versionSupport": true } }, "workspace": { "applyEdit": true, "workspaceEdit": { "documentChanges": true }, "configuration": true } }, "workspaceFolders": [{ "uri": uri(root)?, "name": root.file_name().unwrap_or_default().to_string_lossy() }], "initializationOptions": client.settings }), "initialize")?;
        Ok(client)
    }
    fn send(&mut self, value: Value) -> Result<()> {
        let bytes = serde_json::to_vec(&value)?;
        write!(self.input, "Content-Length: {}\r\n\r\n", bytes.len())?;
        self.input.write_all(&bytes)?;
        self.input.flush()?;
        Ok(())
    }
    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }
    pub fn request(&mut self, method: &str, params: Value, kind: &str) -> Result<()> {
        if !self.ready && method != "initialize" {
            bail!("rust-analyzer is initializing");
        }
        let id = self.next;
        self.next += 1;
        self.pending.insert(id, kind.into());
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = vec![];
        for _ in 0..128 {
            let Ok(message) = self.receive.try_recv() else {
                break;
            };
            let message = match message {
                Ok(v) => v,
                Err(e) => {
                    events.push(Event::Error(e.to_string()));
                    break;
                }
            };
            if let Some(method) = message["method"].as_str() {
                if method == "textDocument/publishDiagnostics"
                    && let Some(uri) = message["params"]["uri"].as_str()
                {
                    self.diagnostics.insert(
                        uri.into(),
                        message["params"]["diagnostics"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default(),
                    );
                }
                if let Some(id) = message.get("id") {
                    let result = match method {
                        "workspace/configuration" => Value::Array(
                            message["params"]["items"]
                                .as_array()
                                .map(|items| items.iter().map(|_| self.settings.clone()).collect())
                                .unwrap_or_default(),
                        ),
                        "workspace/workspaceFolders" => {
                            json!([{ "uri": self.root, "name": "reditor" }])
                        }
                        "workspace/applyEdit" => {
                            events.push(Event::Edit(message["params"]["edit"].clone()));
                            json!({ "applied": false, "failureReason": "Edits are pending user review in reditor" })
                        }
                        _ => Value::Null,
                    };
                    let _ = self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            } else if let Some(id) = message["id"].as_u64()
                && let Some(kind) = self.pending.remove(&id)
            {
                if message.get("error").is_some() {
                    events.push(Event::Error(
                        message["error"]["message"]
                            .as_str()
                            .unwrap_or("LSP error")
                            .into(),
                    ));
                } else if kind == "initialize" {
                    self.ready = true;
                    self.status = "ready".into();
                    let _ = self.notify("initialized", json!({}));
                } else {
                    events.push(Event::Response(kind, message["result"].clone()));
                }
            }
        }
        events
    }
    pub fn sync(&mut self, documents: &[Document]) -> Result<()> {
        if !self.ready {
            return Ok(());
        }
        let mut live = vec![];
        for doc in documents {
            let Some(path) = &doc.path else {
                continue;
            };
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let uri = uri(path)?;
            live.push(uri.clone());
            let old = self.synced.get(&uri).copied();
            if old == Some((doc.id, doc.revision)) {
                continue;
            }
            let version = doc.revision.min(i32::MAX as u64) as i32;
            if old.is_none() {
                self.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": "rust", "version": version, "text": doc.text.to_string() } }))?;
            } else {
                self.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": version, "text": doc.text.to_string() }, "contentChanges": [{ "text": doc.text.to_string() }] }))?;
            }
            self.synced.insert(uri, (doc.id, doc.revision));
        }
        let closed: Vec<String> = self
            .synced
            .keys()
            .filter(|uri| !live.contains(uri))
            .cloned()
            .collect();
        for uri in closed {
            self.notify(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": uri } }),
            )?;
            self.synced.remove(&uri);
        }
        Ok(())
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.notify("exit", Value::Null);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_and_utf16_edits() -> Result<()> {
        let json = json!({"text":"Привет 🦀"}).to_string();
        let mut bytes =
            std::io::Cursor::new(format!("Content-Length: {}\r\n\r\n{}", json.len(), json));
        assert_eq!(read_message(&mut bytes)?.unwrap()["text"], "Привет 🦀");
        let result = edits(
            "🦀 abc\r\n",
            &[
                json!({"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":6}},"newText":"Rust"}),
            ],
        )?;
        assert_eq!(result, "🦀 Rust\r\n");
        assert!(
            offset(
                &ropey::Rope::from_str("🦀"),
                &json!({"line":0,"character":1})
            )
            .is_err()
        );
        let root = tempfile::tempdir()?;
        let p = root.path().join("a #🦀.rs");
        assert_eq!(path(&uri(&p)?)?, p);
        Ok(())
    }
    #[test]
    #[ignore = "requires installed rust-analyzer and rust-src"]
    fn real_rust_analyzer_completion_hover_definition_and_rename() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("src"))?;
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='lsp-test'\nversion='0.1.0'\nedition='2024'\n",
        )?;
        let file = root.path().join("src/main.rs");
        std::fs::write(
            &file,
            "fn greet() -> i32 { 7 }\nfn main() { let value = greet(); println!(\"{}\", value); }\n",
        )?;
        let mut doc = Document::open(&file)?;
        let mut client = Client::start(root.path(), "rust-analyzer")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        while !client.ready() && std::time::Instant::now() < deadline {
            client.poll();
            thread::sleep(std::time::Duration::from_millis(30));
        }
        anyhow::ensure!(client.ready(), "rust-analyzer did not initialize");
        client.sync(std::slice::from_ref(&doc))?;
        // A definition needs the server's background workspace load to finish.
        let position = json!({"line":1,"character":26});
        let uri = uri(&file)?;
        let mut found = false;
        while std::time::Instant::now() < deadline && !found {
            client.request(
                "textDocument/definition",
                json!({"textDocument":{"uri":uri},"position":position}),
                "definition",
            )?;
            let wait = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while std::time::Instant::now() < wait {
                for event in client.poll() {
                    if let Event::Response(kind, result) = event
                        && kind == "definition"
                        && !result.is_null()
                        && result.as_array().is_none_or(|a| !a.is_empty())
                    {
                        found = true;
                    }
                }
                thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        anyhow::ensure!(found, "Definition not returned");
        for (method, kind, extra) in [
            ("textDocument/hover", "hover", json!({})),
            (
                "textDocument/references",
                "references",
                json!({"context":{"includeDeclaration":true}}),
            ),
            (
                "textDocument/rename",
                "rename",
                json!({"newName":"welcome"}),
            ),
            (
                "textDocument/completion",
                "completion",
                json!({"context":{"triggerKind":1}}),
            ),
        ] {
            let mut params = json!({"textDocument":{"uri":uri},"position":position});
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            client.request(method, params, kind)?;
        }
        let mut results = HashMap::new();
        while results.len() < 4 && std::time::Instant::now() < deadline {
            for event in client.poll() {
                if let Event::Response(kind, value) = event {
                    results.insert(kind, value);
                }
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        anyhow::ensure!(
            results.len() == 4,
            "Missing LSP responses: {:?}",
            results.keys()
        );
        anyhow::ensure!(!results["hover"].is_null(), "Empty hover");
        anyhow::ensure!(
            results["references"]
                .as_array()
                .is_some_and(|v| v.len() >= 2),
            "Missing references"
        );
        anyhow::ensure!(
            results["rename"].to_string().contains("welcome"),
            "Missing rename edits"
        );
        anyhow::ensure!(!results["completion"].is_null(), "Missing completions");
        doc.replace("fn main() { let _: i32 = \"broken\"; }\n");
        doc.save(&file, false)?;
        client.sync(std::slice::from_ref(&doc))?;
        client.notify("textDocument/didSave", json!({"textDocument":{"uri":uri}}))?;
        let wait = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < wait
            && client.diagnostics.get(&uri).is_none_or(|v| v.is_empty())
        {
            client.poll();
            thread::sleep(std::time::Duration::from_millis(30));
        }
        anyhow::ensure!(
            client.diagnostics.get(&uri).is_some_and(|v| !v.is_empty()),
            "Missing diagnostics"
        );
        Ok(())
    }
}
