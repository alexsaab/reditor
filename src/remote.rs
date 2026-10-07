use crate::{
    app::App,
    settings,
    studio::{Action, Choice, JobResult},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub const LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ftp,
    Sftp,
    Ftps,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub protocol: Protocol,
    pub host: String,
    pub user: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default = "default_root")]
    pub root: String,
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    #[serde(default)]
    pub key_passphrase_env: Option<String>,
    #[serde(default)]
    pub known_hosts: Option<PathBuf>,
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    #[serde(default)]
    pub implicit_tls: bool,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}
fn default_root() -> String {
    "/".into()
}
fn default_timeout() -> u64 {
    30
}
impl Profile {
    fn port(&self) -> u16 {
        self.port.unwrap_or(match self.protocol {
            Protocol::Sftp => 22,
            Protocol::Ftps if self.implicit_tls => 990,
            _ => 21,
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.host.is_empty()
                && !self
                    .host
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '@' | '\0')),
            "Invalid remote host"
        );
        ensure!(
            !self.user.is_empty() && !self.user.contains(['\r', '\n', '\0', ':']),
            "Invalid remote user"
        );
        ensure!(
            (1..=120).contains(&self.timeout_seconds),
            "timeout_seconds must be 1..120"
        );
        ensure!(self.port() > 0, "Invalid port");
        ensure!(self.root.starts_with('/'), "Remote root must be absolute");
        normalize(&self.root)?;
        Ok(())
    }
    fn same_target(&self, other: &Self) -> bool {
        self.protocol == other.protocol
            && self.host == other.host
            && self.user == other.user
            && self.port() == other.port()
            && self.root == other.root
            && self.implicit_tls == other.implicit_tls
    }
    fn secret(&self, name: &Option<String>) -> Result<Option<String>> {
        name.as_ref()
            .map(|name| {
                std::env::var(name).with_context(|| format!("Set environment variable {name}"))
            })
            .transpose()
    }
    fn remote_path(&self, relative: &str) -> Result<String> {
        let relative = normalize(relative)?;
        let root = normalize(&self.root)?;
        Ok(format!(
            "/{root}{}{}",
            if !root.is_empty() && !relative.is_empty() {
                "/"
            } else {
                ""
            },
            relative
        ))
    }
    fn url(&self, relative: &str, directory: bool) -> Result<url::Url> {
        let mut url = url::Url::parse(if self.protocol == Protocol::Ftps && self.implicit_tls {
            "ftps://localhost"
        } else {
            "ftp://localhost"
        })?;
        url.set_host(Some(&self.host))
            .map_err(|_| anyhow::anyhow!("Invalid host"))?;
        url.set_port(Some(self.port()))
            .map_err(|_| anyhow::anyhow!("Invalid port"))?;
        let remote = self.remote_path(relative)?;
        let mut path = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid URL"))?;
        // A double slash selects an absolute FTP server path, independent of login CWD.
        path.clear().push("");
        for part in remote
            .trim_start_matches('/')
            .split('/')
            .filter(|p| !p.is_empty())
        {
            path.push(part);
        }
        if directory {
            path.push("");
        }
        drop(path);
        url.set_path(&format!("/{}", url.path()));
        Ok(url)
    }
    fn ssh(&self) -> Result<ssh2::Session> {
        self.validate()?;
        let addresses = (self.host.as_str(), self.port()).to_socket_addrs()?;
        let mut socket = None;
        for address in addresses {
            if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
                socket = Some(stream);
                break;
            }
        }
        let socket = socket.context("SSH connection failed")?;
        socket.set_read_timeout(Some(Duration::from_secs(self.timeout_seconds)))?;
        socket.set_write_timeout(Some(Duration::from_secs(self.timeout_seconds)))?;
        let mut session = ssh2::Session::new()?;
        session.set_timeout((self.timeout_seconds * 1000) as u32);
        session.set_tcp_stream(socket);
        session.handshake()?;
        let hosts = self
            .known_hosts
            .clone()
            .unwrap_or(settings::user_dir()?.join("known_hosts"));
        let mut known = session.known_hosts()?;
        known
            .read_file(&hosts, ssh2::KnownHostFileKind::OpenSSH)
            .with_context(|| format!("Read SSH known_hosts: {}", hosts.display()))?;
        let (key, _) = session.host_key().context("SSH host key missing")?;
        ensure!(
            matches!(
                known.check_port(&self.host, self.port(), key),
                ssh2::CheckResult::Match
            ),
            "SSH host key is unknown or changed; verify the server key in {}",
            hosts.display()
        );
        if let Some(key) = &self.key_file {
            let passphrase = self.secret(&self.key_passphrase_env)?;
            session.userauth_pubkey_file(&self.user, None, key, passphrase.as_deref())?;
        } else if let Some(password) = self.secret(&self.password_env)? {
            session.userauth_password(&self.user, &password)?;
        } else {
            session.userauth_agent(&self.user)?;
        }
        ensure!(session.authenticated(), "SSH authentication failed");
        Ok(session)
    }
    fn curl(&self, relative: &str, directory: bool, arguments: &[String]) -> Result<String> {
        self.validate()?;
        let password = self.secret(&self.password_env)?.unwrap_or_default();
        let credentials = format!("{}:{}", self.user, password);
        let config = format!("user = {}\n", quote_config(&credentials));
        let mut command = Command::new("curl");
        command.args([
            "--disable",
            "--config",
            "-",
            "--silent",
            "--show-error",
            "--fail",
            "--globoff",
            "--proto",
            "=ftp,ftps",
            "--connect-timeout",
            "10",
            "--max-time",
            &self.timeout_seconds.to_string(),
            "--max-filesize",
            &LIMIT.to_string(),
        ]);
        if self.protocol == Protocol::Ftps {
            command.arg("--ssl-reqd");
        }
        if let Some(ca) = &self.ca_file {
            command.arg("--cacert").arg(ca);
        }
        command
            .args(arguments)
            .arg("--url")
            .arg(self.url(relative, directory)?.as_str());
        let (ok, out, err) = crate::process::run(
            command,
            config,
            Duration::from_secs(self.timeout_seconds + 2),
        )?;
        if !ok {
            bail!(
                "{}",
                err.trim()
                    .replace(&credentials, "[credentials]")
                    .replace(&password, if password.is_empty() { "" } else { "[secret]" })
            );
        }
        ensure!(
            out.len() < 2 * 1024 * 1024,
            "FTP directory response exceeds 2 MiB"
        );
        Ok(out)
    }
    pub fn list(&self, relative: &str) -> Result<Vec<Entry>> {
        self.validate()?;
        let mut entries = if self.protocol == Protocol::Sftp {
            let session = self.ssh()?;
            let sftp = session.sftp()?;
            sftp.readdir(Path::new(&self.remote_path(relative)?))?
                .into_iter()
                .filter_map(|(path, stat)| {
                    let name = path.file_name()?.to_str()?.to_owned();
                    Some(Entry {
                        name,
                        directory: stat.is_dir(),
                        size: stat.size,
                    })
                })
                .collect()
        } else {
            // RFC 3659 MLSD is unambiguous; older servers fall back to Unix/DOS LIST.
            match self.curl(relative, true, &["--request".into(), "MLSD".into()]) {
                Ok(list) => parse_mlsd(&list).or_else(|_| parse_list(&list))?,
                Err(_) => parse_list(&self.curl(relative, true, &[])?)?,
            }
        };
        entries.retain(|entry| valid_name(&entry.name));
        ensure!(
            entries.len() <= 20_000,
            "Remote directory has more than 20000 entries"
        );
        entries.sort_by(|a, b| b.directory.cmp(&a.directory).then(a.name.cmp(&b.name)));
        Ok(entries)
    }
    pub fn read(&self, relative: &str) -> Result<String> {
        self.validate()?;
        let bytes = if self.protocol == Protocol::Sftp {
            let session = self.ssh()?;
            let sftp = session.sftp()?;
            let mut file = sftp.open(Path::new(&self.remote_path(relative)?))?;
            if let Some(size) = file.stat()?.size {
                ensure!(size <= LIMIT as u64, "Remote file exceeds 8 MiB");
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(LIMIT as u64 + 1)
                .read_to_end(&mut bytes)?;
            bytes
        } else {
            let file = tempfile::NamedTempFile::new()?;
            self.curl(
                relative,
                false,
                &[
                    "--output".into(),
                    file.path().to_string_lossy().into_owned(),
                ],
            )?;
            ensure!(
                fs::metadata(file.path())?.len() <= LIMIT as u64,
                "Remote file exceeds 8 MiB"
            );
            fs::read(file.path())?
        };
        ensure!(bytes.len() <= LIMIT, "Remote file exceeds 8 MiB");
        ensure!(!bytes.contains(&0), "Binary files cannot be edited");
        String::from_utf8(bytes).context("Remote file must be UTF-8")
    }
    pub fn write(&self, relative: &str, text: &str, baseline: Option<&str>) -> Result<()> {
        self.validate()?;
        ensure!(text.len() <= LIMIT, "Remote file exceeds 8 MiB");
        if let Some(baseline) = baseline {
            let current = self.read(relative)?;
            if current == text {
                return Ok(());
            }
            ensure!(
                current == baseline,
                "Remote file changed since download; local copy retained. Reload or compare before saving."
            );
        }
        if self.protocol == Protocol::Sftp {
            let session = self.ssh()?;
            let sftp = session.sftp()?;
            let path = self.remote_path(relative)?;
            let nonce = tempfile::NamedTempFile::new()?;
            let suffix = nonce.path().file_name().unwrap().to_string_lossy();
            let staging = format!("{path}.reditor-{suffix}");
            let result = (|| -> Result<()> {
                let mut file = sftp.open_mode(
                    Path::new(&staging),
                    ssh2::OpenFlags::WRITE | ssh2::OpenFlags::CREATE | ssh2::OpenFlags::EXCLUSIVE,
                    0o600,
                    ssh2::OpenType::File,
                )?;
                file.write_all(text.as_bytes())?;
                if let Ok(stat) = sftp.stat(Path::new(&path)) {
                    sftp.setstat(
                        Path::new(&staging),
                        ssh2::FileStat {
                            perm: stat.perm,
                            size: None,
                            uid: None,
                            gid: None,
                            atime: None,
                            mtime: None,
                        },
                    )?;
                }
                file.close()?;
                if let Some(baseline) = baseline {
                    ensure!(
                        self.read(relative)? == baseline,
                        "Remote file changed during upload; save cancelled"
                    );
                }
                sftp.rename(
                    Path::new(&staging),
                    Path::new(&path),
                    Some(ssh2::RenameFlags::OVERWRITE | ssh2::RenameFlags::ATOMIC),
                )?;
                Ok(())
            })();
            if result.is_err() {
                let _ = sftp.unlink(Path::new(&staging));
            }
            result?;
        } else {
            let mut file = tempfile::NamedTempFile::new()?;
            file.write_all(text.as_bytes())?;
            let suffix = file.path().file_name().unwrap().to_string_lossy();
            let staging = format!("{relative}.reditor-{suffix}");
            self.curl(
                &staging,
                false,
                &[
                    "--upload-file".into(),
                    file.path().to_string_lossy().into_owned(),
                ],
            )?;
            if let Some(baseline) = baseline {
                ensure!(
                    self.read(relative)? == baseline,
                    "Remote file changed during upload; staging file retained"
                );
            }
            self.curl(
                "",
                true,
                &[
                    "--quote".into(),
                    format!("RNFR {}", self.remote_path(&staging)?),
                    "--quote".into(),
                    format!("RNTO {}", self.remote_path(relative)?),
                    "--list-only".into(),
                ],
            )?;
        }
        ensure!(
            self.read(relative)? == text,
            "Remote upload verification failed; local copy retained"
        );
        Ok(())
    }
    pub fn mkdir(&self, relative: &str) -> Result<()> {
        if self.protocol == Protocol::Sftp {
            self.ssh()?
                .sftp()?
                .mkdir(Path::new(&self.remote_path(relative)?), 0o755)?;
        } else {
            self.curl(
                "",
                true,
                &[
                    "--quote".into(),
                    format!("MKD {}", self.remote_path(relative)?),
                    "--list-only".into(),
                ],
            )?;
        }
        Ok(())
    }
}
fn quote_config(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}
pub fn normalize(path: &str) -> Result<String> {
    ensure!(
        !path.contains(['\r', '\n', '\0', '\\', ':']),
        "Invalid remote path"
    );
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(parts.pop().is_some(), "Path escapes remote root");
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\r', '\n', '\0', ':'])
}
#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
    pub size: Option<u64>,
}
fn parse_mlsd(list: &str) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for line in list.lines().filter(|line| !line.trim().is_empty()) {
        let (facts, name) = line.split_once(' ').context("Invalid MLSD response")?;
        let mut entry = Entry {
            name: name.into(),
            directory: false,
            size: None,
        };
        let mut skip = false;
        let mut typed = false;
        for fact in facts.split(';') {
            if let Some((key, value)) = fact.split_once('=') {
                if key.eq_ignore_ascii_case("type") {
                    typed = true;
                    entry.directory = value.eq_ignore_ascii_case("dir");
                    skip = value.eq_ignore_ascii_case("cdir") || value.eq_ignore_ascii_case("pdir");
                }
                if key.eq_ignore_ascii_case("size") {
                    entry.size = value.parse().ok();
                }
            }
        }
        ensure!(typed, "Invalid MLSD response: missing type");
        if !skip {
            entries.push(entry);
        }
    }
    Ok(entries)
}
fn parse_list(list: &str) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for line in list
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with("total "))
    {
        let unix = matches!(line.as_bytes().first(), Some(b'd' | b'-' | b'l'));
        let count = if unix { 8 } else { 3 };
        let mut end = 0;
        let mut fields = Vec::new();
        for _ in 0..count {
            while end < line.len() && line.as_bytes()[end].is_ascii_whitespace() {
                end += 1;
            }
            let start = end;
            while end < line.len() && !line.as_bytes()[end].is_ascii_whitespace() {
                end += 1;
            }
            ensure!(
                end > start,
                "Unsupported FTP LIST format; server should support MLSD"
            );
            fields.push(&line[start..end]);
        }
        let name = line[end..].trim_start();
        let name = if line.starts_with('l') {
            name.split_once(" -> ").map_or(name, |(name, _)| name)
        } else {
            name
        };
        entries.push(Entry {
            name: name.into(),
            directory: if unix {
                line.starts_with('d')
            } else {
                fields[2].eq_ignore_ascii_case("<DIR>")
            },
            size: fields[if unix { 4 } else { 2 }].parse().ok(),
        });
    }
    Ok(entries)
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Connections {
    #[serde(default)]
    connections: BTreeMap<String, Profile>,
}
pub fn profiles(root: &Path) -> Result<BTreeMap<String, Profile>> {
    profiles_from(&settings::user_dir()?, root)
}
fn profiles_from(user: &Path, root: &Path) -> Result<BTreeMap<String, Profile>> {
    fn layer(directory: &Path) -> Result<toml::Value> {
        let mut value = settings::read(&directory.join("connections.toml"))?;
        if let Some(connections) = value.get_mut("connections").and_then(|v| v.as_table_mut()) {
            for (_, profile) in connections.iter_mut() {
                for field in ["key_file", "known_hosts", "ca_file"] {
                    if let Some(value) = profile.get_mut(field) {
                        let path = PathBuf::from(
                            value.as_str().context("Credential path must be a string")?,
                        );
                        let path = if let Ok(rest) = path.strip_prefix("~") {
                            let home = std::env::var_os("HOME")
                                .or_else(|| std::env::var_os("USERPROFILE"))
                                .context("Home missing")?;
                            PathBuf::from(home).join(rest)
                        } else if path.is_relative() {
                            directory.join(path)
                        } else {
                            path
                        };
                        *value = toml::Value::String(path.to_string_lossy().into_owned());
                    }
                }
            }
        }
        Ok(value)
    }
    let mut value = layer(user)?;
    settings::merge(&mut value, layer(&root.join(".reditor"))?);
    let connections: Connections = value.try_into()?;
    for (name, profile) in &connections.connections {
        ensure!(
            valid_name(name)
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
            "Connection name must use letters, digits, - or _"
        );
        profile.validate().with_context(|| name.clone())?;
    }
    Ok(connections.connections)
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Binding {
    pub profile_id: String,
    pub profile: Profile,
    pub relative: String,
    pub baseline: String,
}
#[derive(Default)]
pub struct State {
    pub bindings: BTreeMap<PathBuf, Binding>,
    pub current: Option<(String, String)>,
}
impl State {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(".reditor/remote/index.json");
        let bindings = if path.exists() {
            ensure!(
                fs::metadata(&path)?.len() <= 128 * 1024 * 1024,
                "Remote index exceeds 128 MiB"
            );
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            BTreeMap::new()
        };
        let mut bindings: BTreeMap<PathBuf, Binding> = bindings;
        let cache = root.join(".reditor/remote/files");
        bindings.retain(|path, binding| {
            valid_name(&binding.profile_id)
                && normalize(&binding.relative)
                    .is_ok_and(|relative| path == &cache.join(&binding.profile_id).join(relative))
                && binding.baseline.len() <= LIMIT
        });
        Ok(Self {
            bindings,
            current: None,
        })
    }
    fn save(&self, root: &Path) -> Result<()> {
        let dir = root.join(".reditor/remote");
        fs::create_dir_all(&dir)?;
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        file.write_all(&serde_json::to_vec(&self.bindings)?)?;
        file.persist(dir.join("index.json"))?;
        Ok(())
    }
}
pub enum Outcome {
    List(String, String, Vec<Entry>),
    Download(String, Box<Profile>, String, String),
    Uploaded(PathBuf, String),
    Created(String, String),
}
impl App {
    pub(crate) fn remote_binding(&self, path: &Path) -> Option<&Binding> {
        self.studio.remote.bindings.get(path)
    }
    fn profile(&self, id: &str) -> Result<Profile> {
        profiles(&self.root)?
            .remove(id)
            .with_context(|| format!("Connection not found: {id}"))
    }
    pub(crate) fn remote_command(&mut self, command: &str) -> Result<()> {
        match command {
            "remote.connect" => {
                let profiles = profiles(&self.root)?;
                if profiles.is_empty() {
                    return self.remote_command("remote.settings.project");
                }
                self.select(
                    "remote_connect",
                    profiles
                        .into_iter()
                        .map(|(id, p)| Choice {
                            label: format!(
                                "{id} · {:?} · {}@{}:{}",
                                p.protocol,
                                p.user,
                                p.host,
                                p.port()
                            ),
                            action: Action::RemoteBrowse(id, String::new()),
                        })
                        .collect(),
                );
            }
            "remote.browse" => {
                let (id, path) = self
                    .studio
                    .remote
                    .current
                    .clone()
                    .context(self.i18n.t("remote_not_connected").to_owned())?;
                self.remote_browse(id, path)?;
            }
            "remote.upload" => self.remote_upload_current()?,
            "remote.reload" => {
                let binding = self
                    .doc()
                    .path
                    .as_ref()
                    .and_then(|p| self.remote_binding(p))
                    .cloned()
                    .context(self.i18n.t("remote_not_file").to_owned())?;
                ensure!(
                    !self.doc().dirty() && self.doc().text == binding.baseline.as_str(),
                    "Local changes retained; upload or save a separate copy before reloading"
                );
                self.remote_open(binding.profile_id, binding.relative)?;
            }
            "remote.mkdir" | "remote.new_file" => {
                ensure!(
                    self.studio.remote.current.is_some(),
                    "{}",
                    self.i18n.t("remote_not_connected")
                );
                self.input(
                    if command == "remote.mkdir" {
                        "remote_mkdir"
                    } else {
                        "remote_new_file"
                    },
                    command,
                );
            }
            "remote.settings.project" | "remote.settings.user" => {
                let directory = if command.ends_with("user") {
                    settings::user_dir()?
                } else {
                    self.root.join(".reditor")
                };
                fs::create_dir_all(&directory)?;
                let path = directory.join("connections.toml");
                if !path.exists() {
                    fs::write(
                        &path,
                        include_str!("../bundled/remote/connections.example.toml"),
                    )?;
                }
                self.open(&path)?;
            }
            _ => bail!("Unknown remote command"),
        }
        Ok(())
    }
    pub(crate) fn remote_browse(&mut self, id: String, relative: String) -> Result<()> {
        let profile = self.profile(&id)?;
        let relative = normalize(&relative)?;
        self.job("remote.list", move || {
            Ok(JobResult::Remote(Box::new(Outcome::List(
                id,
                relative.clone(),
                profile.list(&relative)?,
            ))))
        })
    }
    pub(crate) fn remote_open(&mut self, id: String, relative: String) -> Result<()> {
        let profile = self.profile(&id)?;
        let relative = normalize(&relative)?;
        self.job("remote.download", move || {
            let text = profile.read(&relative)?;
            Ok(JobResult::Remote(Box::new(Outcome::Download(
                id,
                Box::new(profile),
                relative,
                text,
            ))))
        })
    }
    pub(crate) fn remote_upload_current(&mut self) -> Result<()> {
        let path = self
            .doc()
            .path
            .clone()
            .context(self.i18n.t("remote_not_file").to_owned())?;
        ensure!(
            self.remote_binding(&path).is_some(),
            "{}",
            self.i18n.t("remote_not_file")
        );
        let text = self.hooks("before_save", &self.doc().text.to_string(), Some(&path))?;
        let text = self.formatter_on_save(&path, &text)?;
        ensure!(text.len() <= LIMIT, "Remote file exceeds 8 MiB");
        self.doc_mut().replace(&text);
        self.doc_mut().save(&path, false)?;
        self.remote_upload_saved(path, text)
    }
    pub(crate) fn remote_upload_saved(&mut self, path: PathBuf, text: String) -> Result<()> {
        if self.job_running() {
            if let Some((_, queued)) = self
                .studio
                .pending_uploads
                .iter_mut()
                .find(|(queued_path, _)| queued_path == &path)
            {
                *queued = text;
            } else {
                ensure!(
                    self.studio.pending_uploads.len() < 1000,
                    "Remote upload queue is full"
                );
                self.studio.pending_uploads.push_back((path, text));
            }
            self.status = self.i18n.t("remote_queued").into();
            return Ok(());
        }
        let binding = self
            .remote_binding(&path)
            .cloned()
            .context(self.i18n.t("remote_not_file").to_owned())?;
        let profile = self.profile(&binding.profile_id)?;
        ensure!(
            profile.same_target(&binding.profile),
            "Connection target changed; reconnect before uploading"
        );
        self.job("remote.upload", move || {
            profile.write(&binding.relative, &text, Some(&binding.baseline))?;
            Ok(JobResult::Remote(Box::new(Outcome::Uploaded(path, text))))
        })
    }
    pub(crate) fn remote_input(&mut self, kind: &str, name: &str) -> Result<()> {
        ensure!(
            valid_name(name),
            "Enter a filename without /, \\ or newlines"
        );
        let name = name.to_owned();
        let (id, directory) = self
            .studio
            .remote
            .current
            .clone()
            .context(self.i18n.t("remote_not_connected").to_owned())?;
        let profile = self.profile(&id)?;
        let relative = normalize(&format!("{directory}/{name}"))?;
        if kind == "remote.mkdir" {
            self.job(kind, move || {
                profile.mkdir(&relative)?;
                Ok(JobResult::Remote(Box::new(Outcome::Created(id, directory))))
            })
        } else {
            // Creating a local buffer never overwrites an existing remote file.
            self.job(kind, move || {
                ensure!(
                    !profile
                        .list(&directory)?
                        .iter()
                        .any(|entry| entry.name == name),
                    "Remote file already exists"
                );
                profile.write(&relative, "", None)?;
                Ok(JobResult::Remote(Box::new(Outcome::Download(
                    id,
                    Box::new(profile),
                    relative,
                    String::new(),
                ))))
            })
        }
    }
    pub(crate) fn remote_result(&mut self, outcome: Outcome) -> Result<()> {
        match outcome {
            Outcome::List(id, relative, entries) => {
                self.studio.remote.current = Some((id.clone(), relative.clone()));
                let mut items = Vec::new();
                if !relative.is_empty() {
                    items.push(Choice {
                        label: "📁 ..".into(),
                        action: Action::RemoteBrowse(
                            id.clone(),
                            relative
                                .rsplit_once('/')
                                .map_or("", |(parent, _)| parent)
                                .into(),
                        ),
                    });
                }
                items.extend(entries.into_iter().map(|entry| {
                    let path = if relative.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{relative}/{}", entry.name)
                    };
                    Choice {
                        label: format!(
                            "{} {}{}",
                            if entry.directory { "📁" } else { "·" },
                            entry.name,
                            entry
                                .size
                                .map_or(String::new(), |size| format!(" · {size} B"))
                        ),
                        action: if entry.directory {
                            Action::RemoteBrowse(id.clone(), path)
                        } else {
                            Action::RemoteOpen(id.clone(), path)
                        },
                    }
                }));
                self.select("remote_browse", items);
                if let Some(panel) = self.studio.panel.as_mut() {
                    panel.title = format!("{} · {id} /{relative}", self.i18n.t("remote_browse"));
                }
            }
            Outcome::Download(id, profile, relative, text) => {
                let base = self.root.join(".reditor/remote/files").join(&id);
                let path = base.join(normalize(&relative)?);
                let old = self.remote_binding(&path).cloned();
                if let Some(document) = self
                    .documents
                    .iter()
                    .find(|d| d.path.as_ref() == Some(&path))
                {
                    ensure!(
                        !document.dirty(),
                        "Unsaved local changes retained; download cancelled"
                    );
                }
                if path.exists() {
                    let old =
                        old.context("Existing cache has no remote baseline; download cancelled")?;
                    ensure!(
                        fs::read_to_string(&path)? == old.baseline,
                        "Local changes retained; download cancelled"
                    );
                }
                fs::create_dir_all(path.parent().context("Invalid remote filename")?)?;
                let canonical_parent = path.parent().unwrap().canonicalize()?;
                let cache = self.root.join(".reditor/remote/files").canonicalize()?;
                ensure!(
                    cache.starts_with(&self.root),
                    "Remote cache escapes workspace"
                );
                ensure!(
                    canonical_parent.starts_with(cache),
                    "Remote cache escapes workspace"
                );
                ensure!(
                    !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
                    "Remote cache cannot be a symlink"
                );
                fs::write(&path, &text)?;
                let path = path.canonicalize()?;
                self.studio.remote.bindings.insert(
                    path.clone(),
                    Binding {
                        profile_id: id.clone(),
                        profile: *profile,
                        relative: relative.clone(),
                        baseline: text.clone(),
                    },
                );
                self.studio.remote.save(&self.root)?;
                // App::open reuses existing tabs; refresh only after checking for local edits.
                if let Some(index) = self
                    .documents
                    .iter()
                    .position(|d| d.path.as_ref() == Some(&path))
                {
                    self.documents[index] = crate::document::Document::open(&path)?;
                }
                self.open(&path)?;
                self.studio.remote.current = Some((
                    id.clone(),
                    relative
                        .rsplit_once('/')
                        .map_or("", |(parent, _)| parent)
                        .into(),
                ));
                self.status = format!("{} · {id} /{relative}", self.i18n.t("remote_opened"));
                self.dialog = None;
            }
            Outcome::Uploaded(path, text) => {
                if let Some(binding) = self.studio.remote.bindings.get_mut(&path) {
                    binding.baseline = text;
                }
                self.studio.remote.save(&self.root)?;
                self.status = self.i18n.t("remote_uploaded").into();
                self.dialog = None;
            }
            Outcome::Created(id, directory) => self.remote_browse(id, directory)?,
        }
        self.status_error = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile(protocol: Protocol) -> Profile {
        Profile {
            protocol,
            host: "localhost".into(),
            user: "demo".into(),
            port: None,
            root: "/var/www".into(),
            password_env: None,
            key_file: None,
            key_passphrase_env: None,
            known_hosts: None,
            ca_file: None,
            implicit_tls: false,
            timeout_seconds: 5,
        }
    }
    #[test]
    fn paths_credentials_and_directory_formats() -> Result<()> {
        assert!(normalize("../escape").is_err());
        assert!(normalize("C:/escape").is_err());
        assert!(normalize("name\r\nDELE file").is_err());
        assert_eq!(normalize("folder/./child/../🦀.php")?, "folder/🦀.php");
        let p = profile(Protocol::Ftps);
        assert_eq!(
            p.url("dir/100% #?.php", false)?.as_str(),
            "ftp://localhost//var/www/dir/100%25%20%23%3F.php"
        );
        let mut p = p;
        p.implicit_tls = true;
        assert!(
            p.url("", true)?
                .as_str()
                .starts_with("ftps://localhost:990/")
        );
        assert_eq!(quote_config("u:p\"\nurl=x"), "\"u:p\\\"\\nurl=x\"");
        let entries = parse_mlsd(
            "type=cdir; .\r\ntype=dir; nested folder\r\ntype=file;size=5; index.php\r\n",
        )?;
        assert_eq!(entries.len(), 2);
        assert!(entries[0].directory);
        assert_eq!(entries[0].name, "nested folder");
        assert_eq!(entries[1].size, Some(5));
        assert!(parse_mlsd("-rw-r--r-- 1 x x 5 Jan 1 12:00 a.php").is_err());
        let entries = parse_list(
            "drwxr-xr-x 2 u g 4096 Sep 29 12:00 nested folder\n-rw-r--r-- 1 u g 5 Sep 29 12:00 index.php\n",
        )?;
        assert!(entries[0].directory);
        assert_eq!(entries[0].name, "nested folder");
        let entries = parse_list(
            "09-29-26  12:00PM       <DIR>          nested folder\n09-29-26  12:00PM                   5 index.php\n",
        )?;
        assert!(entries[0].directory);
        assert_eq!(entries[1].name, "index.php");
        Ok(())
    }
    #[test]
    fn connection_overrides_keep_path_origins() -> Result<()> {
        let user = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        fs::create_dir(project.path().join(".reditor"))?;
        fs::write(
            user.path().join("connections.toml"),
            "[connections.site]\nprotocol='sftp'\nhost='localhost'\nuser='demo'\nkey_file='keys/private'\nknown_hosts='known_hosts'\n",
        )?;
        fs::write(
            project.path().join(".reditor/connections.toml"),
            "[connections.site]\nroot='/project'\nknown_hosts='hosts'\n",
        )?;
        let p = profiles_from(user.path(), project.path())?
            .remove("site")
            .unwrap();
        assert_eq!(p.key_file, Some(user.path().join("keys/private")));
        assert_eq!(p.known_hosts, Some(project.path().join(".reditor/hosts")));
        assert_eq!(p.root, "/project");
        Ok(())
    }
    #[test]
    fn remote_cache_protects_local_edits_and_restores_bindings() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut app = App::new(root.path().into(), crate::i18n::Language::Ru, vec![])?;
        app.remote_result(Outcome::Download(
            "site".into(),
            Box::new(profile(Protocol::Ftp)),
            "folder/index.php".into(),
            "<?php echo 'hello';".into(),
        ))?;
        let path = app.doc().path.clone().unwrap();
        assert!(app.remote_binding(&path).is_some());
        app.doc_mut().insert("changed");
        assert!(
            app.remote_result(Outcome::Download(
                "site".into(),
                Box::new(profile(Protocol::Ftp)),
                "folder/index.php".into(),
                "other".into()
            ))
            .is_err()
        );
        assert!(app.doc().text.to_string().contains("changed"));
        let state = State::load(&app.root)?;
        assert_eq!(state.bindings[&path].baseline, "<?php echo 'hello';");
        let mut changed = profile(Protocol::Ftp);
        changed.host = "other".into();
        assert!(!state.bindings[&path].profile.same_target(&changed));
        Ok(())
    }
    #[test]
    #[ignore = "requires scripts/smoke_remote.py local FTP/FTPS/SFTP fixtures"]
    fn remote_protocol_roundtrip() -> Result<()> {
        let root = PathBuf::from(std::env::var("REDITOR_REMOTE_TEST_ROOT")?);
        let profiles = profiles(&root)?;
        ensure!(profiles.len() >= 3, "Need FTP, FTPS and SFTP fixtures");
        for (id, profile) in &profiles {
            if id == "sftp_key" {
                assert_eq!(profile.read("sample.php")?, "<?php echo 'changed';\n");
                println!("Remote protocol passed: {id}");
                continue;
            }
            let entries = profile.list("").with_context(|| format!("{id} listing"))?;
            assert!(entries.iter().any(|e| e.name == "sample.php"));
            let original = profile.read("sample.php")?;
            assert_eq!(original, "<?php echo 'Привет 🦀';\n");
            profile
                .write("sample.php", "<?php echo 'changed';\n", Some(&original))
                .with_context(|| format!("{id} upload"))?;
            assert!(
                profile
                    .write("sample.php", "must-not-overwrite", Some(&original))
                    .is_err()
            );
            assert_eq!(profile.read("sample.php")?, "<?php echo 'changed';\n");
            profile.mkdir("nested space")?;
            assert!(
                profile
                    .list("")?
                    .iter()
                    .any(|e| e.name == "nested space" && e.directory)
            );
            profile.write("nested space/100% 🦀.txt", "UTF-8 ✅", None)?;
            assert_eq!(profile.read("nested space/100% 🦀.txt")?, "UTF-8 ✅");
            if profile.protocol == Protocol::Ftps {
                let mut untrusted = profile.clone();
                untrusted.ca_file = None;
                assert!(
                    untrusted.list("").is_err(),
                    "Self-signed FTPS certificate must be rejected without its trusted CA"
                );
            }
            if profile.protocol == Protocol::Sftp {
                let mut wrong = profile.clone();
                wrong.known_hosts = Some(root.join("wrong_known_hosts"));
                assert!(wrong.list("").is_err());
            }
            println!("Remote protocol passed: {id}");
        }
        // Exercise Lua -> host -> download -> Ctrl+S -> upload, with an upload queued while busy.
        let mut app = App::new(
            root.clone(),
            crate::i18n::Language::Ru,
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled/remote")],
        )?;
        app.command("remote.connect")?;
        assert!(
            app.studio
                .panel
                .as_ref()
                .is_some_and(|p| !p.items.is_empty())
        );
        app.remote_open("sftp".into(), "sample.php".into())?;
        let wait = |app: &mut App| -> Result<()> {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while (app.job_running() || !app.studio.pending_uploads.is_empty())
                && std::time::Instant::now() < deadline
            {
                app.poll_studio();
                std::thread::sleep(Duration::from_millis(10));
            }
            ensure!(!app.job_running(), "Remote job timeout");
            ensure!(!app.status_error, "{}", app.status);
            Ok(())
        };
        wait(&mut app)?;
        let path = app.doc().path.clone().unwrap();
        app.doc_mut().replace("<?php echo 'from editor';\n");
        app.save(&path, false)?;
        app.doc_mut().replace("<?php echo 'queued edit';\n");
        app.save(&path, false)?;
        wait(&mut app)?;
        assert_eq!(
            profiles["sftp"].read("sample.php")?,
            "<?php echo 'queued edit';\n"
        );
        assert_eq!(
            State::load(&root)?.bindings[&path].baseline,
            "<?php echo 'queued edit';\n"
        );
        Ok(())
    }
}
