use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    io::{Read, Write},
    path::Path,
    sync::mpsc::{self, Receiver},
    thread,
};

pub struct TerminalSession {
    pub title: String,
    pub parser: vt100::Parser,
    pub exit: Option<String>,
    pub exit_code: Option<u32>,
    pub output: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    receive: Receiver<Vec<u8>>,
    size: (u16, u16),
}
impl TerminalSession {
    pub fn spawn(title: String, root: &Path, program: &str, args: &[String]) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows: 12,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(root);
        command.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| program.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (send, receive) = mpsc::sync_channel(64);
        thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 || send.send(bytes[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            title,
            parser: vt100::Parser::new(12, 80, 10000),
            exit: None,
            exit_code: None,
            output: String::new(),
            master: pair.master,
            writer,
            child,
            receive,
            size: (12, 80),
        })
    }
    pub fn poll(&mut self) {
        for _ in 0..64 {
            let Ok(bytes) = self.receive.try_recv() else {
                break;
            };
            self.parser.process(&bytes);
            self.output.push_str(&String::from_utf8_lossy(&bytes));
            if self.output.len() > 2 * 1024 * 1024 {
                let mut cut = self.output.len() - 1024 * 1024;
                while !self.output.is_char_boundary(cut) {
                    cut += 1;
                }
                self.output.drain(..cut);
            }
            if bytes.windows(4).any(|s| s == b"\x1b[6n") {
                let (row, col) = self.parser.screen().cursor_position();
                let _ = self.write(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
        }
        if self.exit.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit = Some(status.to_string());
            self.exit_code = Some(status.exit_code());
        }
    }
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let size = (rows.max(1), cols.max(1));
        if self.size != size {
            let _ = self.master.resize(PtySize {
                rows: size.0,
                cols: size.1,
                pixel_width: 0,
                pixel_height: 0,
            });
            self.parser.screen_mut().set_size(size.0, size.1);
            self.size = size;
        }
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.parser.screen_mut().set_scrollback(0);
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }
    pub fn paste(&mut self, text: &str) -> Result<()> {
        if self.parser.screen().bracketed_paste() {
            self.write(b"\x1b[200~")?;
            self.write(text.as_bytes())?;
            self.write(b"\x1b[201~")
        } else {
            self.write(text.as_bytes())
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Result<()> {
        let mut bytes = Vec::new();
        match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if c.is_ascii() {
                    bytes.push(c.to_ascii_uppercase() as u8 & 0x1f);
                }
            }
            KeyCode::Char(c) => bytes.extend(c.to_string().as_bytes()),
            KeyCode::Enter => bytes.push(b'\r'),
            KeyCode::Backspace => bytes.push(127),
            KeyCode::Tab => bytes.push(b'\t'),
            KeyCode::Esc => bytes.push(27),
            KeyCode::BackTab => bytes.extend(b"\x1b[Z"),
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                let direction = match key.code {
                    KeyCode::Up => b'A',
                    KeyCode::Down => b'B',
                    KeyCode::Right => b'C',
                    _ => b'D',
                };
                bytes.extend(if self.parser.screen().application_cursor() {
                    b"\x1bO"
                } else {
                    b"\x1b["
                });
                bytes.push(direction);
            }
            KeyCode::Home => bytes.extend(b"\x1b[H"),
            KeyCode::End => bytes.extend(b"\x1b[F"),
            KeyCode::Delete => bytes.extend(b"\x1b[3~"),
            KeyCode::Insert => bytes.extend(b"\x1b[2~"),
            KeyCode::PageUp if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll(10);
                return Ok(());
            }
            KeyCode::PageDown if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll(-10);
                return Ok(());
            }
            KeyCode::PageUp => bytes.extend(b"\x1b[5~"),
            KeyCode::PageDown => bytes.extend(b"\x1b[6~"),
            KeyCode::F(n) => bytes.extend(
                match n {
                    1 => "\x1bOP",
                    2 => "\x1bOQ",
                    3 => "\x1bOR",
                    4 => "\x1bOS",
                    5 => "\x1b[15~",
                    6 => "\x1b[17~",
                    7 => "\x1b[18~",
                    8 => "\x1b[19~",
                    9 => "\x1b[20~",
                    10 => "\x1b[21~",
                    11 => "\x1b[23~",
                    _ => "\x1b[24~",
                }
                .as_bytes(),
            ),
            _ => {}
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            bytes.insert(0, 27);
        }
        self.write(&bytes)
    }
    pub fn scroll(&mut self, lines: i32) {
        let old = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(old.saturating_add_signed(lines as isize));
    }
    pub fn stop(&mut self) {
        if self.exit.is_none() {
            #[cfg(unix)]
            if let Some(pid) = self.child.process_id() {
                // The PTY child owns its own session/process group.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.poll();
        }
    }
}
impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn pty_accepts_interactive_input_unicode_and_resize() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut terminal = TerminalSession::spawn(
            "test".into(),
            root.path(),
            "/bin/sh",
            &[
                "-c".into(),
                r#"read value; printf '\033[32m%s\033[0m\n' "$value""#.into(),
            ],
        )?;
        terminal.resize(10, 100);
        terminal.paste("Привет Rust\n")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while terminal.exit.is_none() && std::time::Instant::now() < deadline {
            terminal.poll();
            thread::sleep(std::time::Duration::from_millis(10));
        }
        terminal.poll();
        assert!(terminal.parser.screen().contents().contains("Привет Rust"));
        assert!(terminal.exit.is_some());
        Ok(())
    }
}
