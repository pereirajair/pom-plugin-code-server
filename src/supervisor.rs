//! Supervises the official code-server release next to the plugin host.
//!
//! The package only pins the release (URL, SHA-256, version). On the first
//! activation the archive is downloaded from the official GitHub release,
//! verified against the pinned digest and unpacked once per checksum, with the
//! progress published as `installing`. User settings/extensions live in
//! `data/`, separately from the replaceable runtime. The launcher and
//! code-server never inherit the plugin host's IPC stdin/stdout or its private
//! environment; the POM gateway key reaches only the launcher, which keeps it
//! on the server side of its model facade.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const PLUGIN_DIR: &str = "code_server";

/// The POM's OpenAI-compatible endpoint and the key minted for this plugin.
#[derive(Clone, PartialEq)]
pub struct Gateway {
    pub base_url: String,
    pub api_key: String,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Gateway")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Configuration {
    pub workspace_root: Option<PathBuf>,
    /// Absent on POMs that do not share a gateway: the editor still runs,
    /// only the chat has no POM models.
    pub gateway: Option<Gateway>,
}

/// `{"gateway": {"openai_base_url": "...", "api_key": "..."}}`, when usable.
fn gateway_from(request: &Value) -> Option<Gateway> {
    let gateway = request.get("gateway")?;
    let base_url = gateway
        .get("openai_base_url")?
        .as_str()?
        .trim()
        .trim_end_matches('/');
    let api_key = gateway.get("api_key")?.as_str()?.trim();
    let scheme_ok = base_url.starts_with("http://") || base_url.starts_with("https://");
    (scheme_ok && !api_key.is_empty() && !api_key.chars().any(char::is_whitespace)).then(|| {
        Gateway {
            base_url: base_url.to_owned(),
            api_key: api_key.to_owned(),
        }
    })
}

impl Configuration {
    pub fn from_configure(request: &Value) -> Result<Self, String> {
        let workspace_root = match request.get("workspace_root") {
            None | Some(Value::Null) => None,
            Some(Value::String(path)) if path.trim().is_empty() => None,
            Some(Value::String(path)) => {
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    return Err("workspace_root must be an absolute path".into());
                }
                Some(path)
            }
            Some(_) => return Err("workspace_root must be a string or null".into()),
        };
        Ok(Self {
            workspace_root,
            gateway: gateway_from(request),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Unconfigured,
    Starting,
    /// Downloading the pinned release; `total` is 0 when unknown.
    Installing {
        downloaded: u64,
        total: u64,
        version: String,
    },
    Ready {
        port: u16,
        token: String,
        detail: Value,
    },
    Failed(String),
}

impl Status {
    pub fn to_json(&self) -> Value {
        match self {
            Status::Unconfigured | Status::Starting => json!({"status": "starting"}),
            Status::Installing {
                downloaded,
                total,
                version,
            } => json!({
                "status": "installing",
                "detail": {"downloaded": downloaded, "total": total, "version": version}
            }),
            Status::Ready { detail, .. } => json!({"status": "ready", "detail": detail}),
            Status::Failed(error) => json!({"status": "error", "error": error}),
        }
    }

    pub fn upstream_json(&self) -> Value {
        match self {
            Status::Ready { port, token, .. } => {
                json!({"status": "ready", "port": port, "token": token})
            }
            Status::Failed(error) => json!({"status": "error", "error": error}),
            Status::Unconfigured | Status::Starting | Status::Installing { .. } => {
                json!({"status": "starting"})
            }
        }
    }
}

struct Process {
    child: Child,
    stdin: ChildStdin,
}

/// The pinned official release this package downloads.
#[derive(Debug, Clone, Copy)]
pub struct Release {
    pub url: &'static str,
    pub size: u64,
    pub checksum: &'static str,
    pub version: &'static str,
    pub server_root: &'static str,
}

pub struct Supervisor {
    release: Release,
    launcher: &'static str,
    status: Mutex<Status>,
    process: Mutex<Option<Process>>,
    configuration: Mutex<Option<Configuration>>,
    runtime_lock: Mutex<()>,
    generation: AtomicU64,
}

impl Supervisor {
    pub fn new(release: Release, launcher: &'static str) -> Arc<Self> {
        Arc::new(Self {
            release,
            launcher,
            status: Mutex::new(Status::Unconfigured),
            process: Mutex::new(None),
            configuration: Mutex::new(None),
            runtime_lock: Mutex::new(()),
            generation: AtomicU64::new(0),
        })
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or(Status::Starting)
    }

    fn set_status(&self, generation: u64, status: Status) {
        if self.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    /// Starts again with the last configuration, e.g. after a failed download.
    pub fn retry(self: &Arc<Self>) -> bool {
        let configuration = self
            .configuration
            .lock()
            .ok()
            .and_then(|current| current.clone());
        match configuration {
            Some(configuration) => {
                self.start(configuration, true);
                true
            }
            None => false,
        }
    }

    /// Starts/restarts after POM configuration changes; startup never blocks IPC.
    pub fn configure(self: &Arc<Self>, configuration: Configuration) {
        self.start(configuration, false);
    }

    fn start(self: &Arc<Self>, configuration: Configuration, force: bool) {
        let generation = {
            let Ok(mut current) = self.configuration.lock() else {
                return;
            };
            if !force
                && current.as_ref() == Some(&configuration)
                && !matches!(self.status(), Status::Failed(_))
            {
                return;
            }
            *current = Some(configuration.clone());
            self.stop();
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            if let Ok(mut status) = self.status.lock() {
                *status = Status::Starting;
            }
            generation
        };
        let worker = Arc::clone(self);
        thread::spawn(move || {
            if let Err(error) = worker.run(generation, &configuration) {
                worker.stop_generation(generation);
                worker.set_status(generation, Status::Failed(error));
            }
        });
    }

    fn run(&self, generation: u64, configuration: &Configuration) -> Result<(), String> {
        let release = self.release;
        if release.url.is_empty() || release.server_root.is_empty() || release.version.is_empty() {
            return Err(
                "this build has no pinned code-server release; package it with scripts/package.sh"
                    .into(),
            );
        }
        let base = plugin_directory()?;
        let data = base.join("data");
        fs::create_dir_all(&data).map_err(|error| format!("{}: {error}", data.display()))?;
        let workspace = configuration
            .workspace_root
            .clone()
            .unwrap_or_else(|| data.join("workspace"));
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("{}: {error}", workspace.display()))?;

        let runtime = {
            let _guard = self
                .runtime_lock
                .lock()
                .map_err(|_| "code-server runtime lock is unavailable")?;
            if self.generation.load(Ordering::SeqCst) != generation {
                return Ok(());
            }
            install_runtime(
                &base.join("runtime"),
                &release,
                self.launcher,
                |downloaded, total| {
                    self.set_status(
                        generation,
                        Status::Installing {
                            downloaded,
                            total,
                            version: release.version.to_owned(),
                        },
                    );
                    self.generation.load(Ordering::SeqCst) == generation
                },
            )?
        };
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        let code_root = runtime.join(release.server_root);
        let node = code_root
            .join("lib")
            .join(if cfg!(windows) { "node.exe" } else { "node" });
        if !node.is_file() {
            return Err(format!(
                "bundled Node runtime is missing: {}",
                node.display()
            ));
        }

        let mut command = Command::new(&node);
        command
            .arg(runtime.join("launcher.mjs"))
            .current_dir(&runtime)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| system_path().into()),
            )
            .env("HOME", data.join("home"))
            .env("USERPROFILE", data.join("home"))
            .env("POM_CODE_SERVER_ROOT", &code_root)
            .env("POM_CODE_SERVER_DATA_DIR", &data)
            .env("POM_CODE_SERVER_WORKSPACE", &workspace)
            .env("POM_CODE_SERVER_VERSION", release.version)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for name in [
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TZ",
            "SYSTEMROOT",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if !cfg!(windows) {
            command.env("POM_CODE_SERVER_SHELL", "/bin/bash");
        }
        // The launcher serves POM models to the editor's chat; the key stays
        // in its process and never reaches code-server or the browser.
        if let Some(gateway) = &configuration.gateway {
            command
                .env("POM_GATEWAY_BASE_URL", &gateway.base_url)
                .env("POM_GATEWAY_API_KEY", &gateway.api_key);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("start code-server launcher: {error}"))?;
        let stdin = child.stdin.take().ok_or("launcher stdin is not piped")?;
        let stdout = child.stdout.take().ok_or("launcher stdout is not piped")?;
        {
            let Ok(mut process) = self.process.lock() else {
                drop(stdin);
                let _ = child.kill();
                return Err("could not store code-server process".into());
            };
            if self.generation.load(Ordering::SeqCst) != generation {
                drop(stdin);
                let _ = child.kill();
                return Ok(());
            }
            *process = Some(Process { child, stdin });
        }

        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(|error| format!("read code-server status: {error}"))?;
        let reply: Value = serde_json::from_str(line.trim()).map_err(|_| {
            "the code-server launcher exited before announcing its proxy".to_owned()
        })?;
        if reply["status"].as_str() != Some("ready") {
            return Err(reply["error"]
                .as_str()
                .unwrap_or("code-server launcher failed")
                .to_owned());
        }
        let port = reply["port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)
            .ok_or("launcher reported an invalid proxy port")?;
        let token = reply["token"]
            .as_str()
            .ok_or("launcher reported no proxy token")?;
        let detail = json!({"version": release.version});
        self.set_status(
            generation,
            Status::Ready {
                port,
                token: token.to_owned(),
                detail,
            },
        );
        self.watch(generation);
        Ok(())
    }

    fn watch(&self, generation: u64) {
        loop {
            thread::sleep(Duration::from_secs(1));
            if self.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let Ok(mut process) = self.process.lock() else {
                return;
            };
            let Some(running) = process.as_mut() else {
                return;
            };
            if let Ok(Some(exit)) = running.child.try_wait() {
                *process = None;
                drop(process);
                self.set_status(
                    generation,
                    Status::Failed(format!("code-server launcher stopped ({exit})")),
                );
                return;
            }
        }
    }

    fn stop_generation(&self, generation: u64) {
        let process = self.process.lock().ok().and_then(|mut process| {
            if self.generation.load(Ordering::SeqCst) == generation {
                process.take()
            } else {
                None
            }
        });
        Self::stop_process(process);
    }

    pub fn stop(&self) {
        let process = self
            .process
            .lock()
            .ok()
            .and_then(|mut process| process.take());
        Self::stop_process(process);
    }

    fn stop_process(process: Option<Process>) {
        if let Some(mut process) = process {
            drop(process.stdin);
            for _ in 0..30 {
                if matches!(process.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn system_path() -> &'static str {
    if cfg!(windows) {
        "C:\\Windows\\System32;C:\\Windows"
    } else {
        "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    }
}

fn plugin_directory() -> Result<PathBuf, String> {
    let parent = std::env::var_os("POM_PLUGIN_DB")
        .map(PathBuf::from)
        .and_then(|database| database.parent().map(Path::to_path_buf))
        .filter(|parent| !parent.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .ok_or("could not resolve the plugin data directory")?;
    Ok(parent.join(PLUGIN_DIR))
}

fn runtime_ready(target: &Path, server_root: &str) -> bool {
    target.join(".complete").is_file()
        && target.join(server_root).join("lib").is_dir()
        && target.join("launcher.mjs").is_file()
}

fn validate_release(checksum: &str, server_root: &str) -> Result<(), String> {
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("code-server archive checksum is invalid".into());
    }
    if server_root.is_empty() || Path::new(server_root).components().count() != 1 {
        return Err("code-server release directory is invalid".into());
    }
    Ok(())
}

/// Downloads `url` into `destination`, reporting progress. `progress` returns
/// false to abandon the download (a newer configuration replaced this one).
pub fn download(
    url: &str,
    destination: &Path,
    expected_size: u64,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<(), String> {
    let mut agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60));
    if let Some(proxy) = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .and_then(|value| ureq::Proxy::new(value).ok())
    {
        agent = agent.proxy(proxy);
    }
    let response = agent
        .build()
        .get(url)
        .call()
        .map_err(|error| format!("download code-server: {error}"))?;
    let total = response
        .header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(expected_size);
    let mut reader = response.into_reader();
    let mut file = fs::File::create(destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut downloaded = 0_u64;
    let mut last_report = std::time::Instant::now();
    if !progress(0, total) {
        return Err("download cancelled".into());
    }
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("download code-server: {error}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| format!("write code-server download: {error}"))?;
        downloaded += read as u64;
        if last_report.elapsed() >= Duration::from_millis(250) {
            last_report = std::time::Instant::now();
            if !progress(downloaded, total) {
                return Err("download cancelled".into());
            }
        }
    }
    file.sync_all()
        .map_err(|error| format!("write code-server download: {error}"))?;
    progress(downloaded, total.max(downloaded));
    Ok(())
}

/// Makes sure the pinned release is unpacked: reuses a complete runtime,
/// otherwise downloads, verifies and unpacks it.
pub fn install_runtime(
    root: &Path,
    release: &Release,
    launcher: &str,
    progress: impl FnMut(u64, u64) -> bool,
) -> Result<PathBuf, String> {
    validate_release(release.checksum, release.server_root)?;
    let target = root.join(&release.checksum[..16]);
    if runtime_ready(&target, release.server_root) {
        return Ok(target);
    }
    fs::create_dir_all(root).map_err(|error| format!("{}: {error}", root.display()))?;
    let archive = root.join(format!(".{}.tar.gz", &release.checksum[..16]));
    let reusable = fs::read(&archive)
        .map(|bytes| format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(release.checksum))
        .unwrap_or(false);
    if !reusable {
        download(release.url, &archive, release.size, progress)?;
    }
    let result = unpack_archive_file(
        root,
        &archive,
        release.checksum,
        release.server_root,
        launcher,
    );
    let _ = fs::remove_file(&archive);
    result
}

/// Verify and atomically unpack an archive file once per content digest.
pub fn unpack_archive_file(
    root: &Path,
    archive: &Path,
    checksum: &str,
    server_root: &str,
    launcher: &str,
) -> Result<PathBuf, String> {
    validate_release(checksum, server_root)?;
    let id = &checksum[..16];
    let target = root.join(id);
    if runtime_ready(&target, server_root) {
        return Ok(target);
    }
    let mut hasher = Sha256::new();
    let mut file =
        fs::File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    std::io::copy(&mut file, &mut hasher)
        .map_err(|error| format!("read code-server archive: {error}"))?;
    if !format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(checksum) {
        return Err("code-server archive checksum mismatch".into());
    }
    fs::create_dir_all(root).map_err(|error| format!("{}: {error}", root.display()))?;
    let partial = root.join(format!(".{id}.partial"));
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    let file =
        fs::File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    let mut unpacker = tar::Archive::new(flate2::read::GzDecoder::new(BufReader::new(file)));
    unpacker.set_preserve_permissions(true);
    unpacker
        .unpack(&partial)
        .map_err(|error| format!("unpack official code-server release: {error}"))?;
    if !partial.join(server_root).join("lib").is_dir() {
        let _ = fs::remove_dir_all(&partial);
        return Err(format!(
            "official release does not contain {server_root}/lib"
        ));
    }
    fs::write(partial.join("launcher.mjs"), launcher)
        .map_err(|error| format!("write code-server launcher: {error}"))?;
    fs::write(partial.join(".complete"), format!("{checksum}\n"))
        .map_err(|error| format!("mark code-server runtime: {error}"))?;
    fs::rename(&partial, &target)
        .map_err(|error| format!("install code-server runtime: {error}"))?;
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name != id && !name.to_string_lossy().ends_with(".tar.gz") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn test_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pom-code-server-{stamp}"))
    }

    #[test]
    fn configuration_accepts_the_optional_absolute_shared_workspace() {
        assert_eq!(
            Configuration::from_configure(&json!({}))
                .unwrap()
                .workspace_root,
            None
        );
        let workspace = std::env::temp_dir().join("pom-code-server-workspace");
        let workspace_text = workspace.to_string_lossy().into_owned();
        let parsed =
            Configuration::from_configure(&json!({"workspace_root": workspace_text})).unwrap();
        assert_eq!(parsed.workspace_root, Some(workspace));
        assert!(Configuration::from_configure(&json!({"workspace_root": 42})).is_err());
        assert!(Configuration::from_configure(&json!({"workspace_root": "relative"})).is_err());
    }

    #[test]
    fn configuration_reads_the_pom_gateway_and_hides_its_key() {
        let parsed = Configuration::from_configure(&json!({
            "gateway": {"openai_base_url": "http://127.0.0.1:8080/v1/", "api_key": "sk-pom"}
        }))
        .unwrap();
        let gateway = parsed.gateway.clone().unwrap();
        assert_eq!(gateway.base_url, "http://127.0.0.1:8080/v1");
        assert_eq!(gateway.api_key, "sk-pom");
        assert!(!format!("{parsed:?}").contains("sk-pom"));
        for bad in [
            json!({"gateway": {"openai_base_url": "file:///etc", "api_key": "k"}}),
            json!({"gateway": {"openai_base_url": "http://x/v1", "api_key": " "}}),
            json!({"gateway": {"openai_base_url": "http://x/v1"}}),
        ] {
            assert!(Configuration::from_configure(&bad)
                .unwrap()
                .gateway
                .is_none());
        }
    }

    #[test]
    fn installing_status_reports_progress_but_proxies_nothing() {
        let installing = Status::Installing {
            downloaded: 10,
            total: 100,
            version: "4.1".into(),
        };
        assert_eq!(
            installing.to_json(),
            json!({"status": "installing", "detail": {"downloaded": 10, "total": 100, "version": "4.1"}})
        );
        assert_eq!(installing.upstream_json(), json!({"status": "starting"}));
    }

    #[test]
    fn the_release_is_downloaded_verified_and_reused() {
        use std::io::{Read as _, Write as _};
        let root = test_root();
        let bytes = archive(&[("code-server-dl/lib/node", b"node")]);
        let digest: &'static str =
            Box::leak(format!("{:x}", Sha256::digest(&bytes)).into_boxed_str());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let served = bytes.clone();
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    served.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&served).unwrap();
            }
        });
        let url: &'static str =
            Box::leak(format!("http://{address}/release.tar.gz").into_boxed_str());
        let release = Release {
            url,
            size: bytes.len() as u64,
            checksum: digest,
            version: "4.1",
            server_root: "code-server-dl",
        };
        let mut reports = Vec::new();
        let runtime = install_runtime(&root, &release, "// launcher", |done, total| {
            reports.push((done, total));
            true
        })
        .unwrap();
        assert!(runtime.join("code-server-dl/lib/node").is_file());
        assert_eq!(
            reports.last(),
            Some(&(bytes.len() as u64, bytes.len() as u64))
        );
        // A complete runtime is reused without downloading again.
        assert_eq!(
            install_runtime(&root, &release, "", |_, _| panic!("no download")).unwrap(),
            runtime
        );

        // A tampered download is refused.
        let bad = Release {
            checksum: Box::leak("0".repeat(64).into_boxed_str()),
            ..release
        };
        assert!(install_runtime(&test_root(), &bad, "", |_, _| true).is_err());
        let _ = server.join();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_exposes_neither_the_proxy_token_nor_runtime_address_to_the_screen() {
        let ready = Status::Ready {
            port: 48123,
            token: "secret-token-value".into(),
            detail: json!({"version": "4.1.2"}),
        };
        assert_eq!(
            ready.to_json(),
            json!({"status": "ready", "detail": {"version": "4.1.2"}})
        );
        assert_eq!(ready.upstream_json()["token"], "secret-token-value");
        assert_eq!(
            Status::Starting.upstream_json(),
            json!({"status": "starting"})
        );
    }

    fn unpack_runtime(
        root: &Path,
        bytes: &[u8],
        digest: &str,
        server_root: &str,
        launcher: &str,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(root).unwrap();
        let file = root.with_extension(format!("{}.tar.gz", &digest[..8]));
        fs::write(&file, bytes).unwrap();
        let result = unpack_archive_file(root, &file, digest, server_root, launcher);
        let _ = fs::remove_file(file);
        result
    }

    #[test]
    fn runtime_is_verified_unpacked_once_and_replaced_on_checksum_change() {
        let root = test_root();
        let first = archive(&[
            ("code-server-test/lib/node", b"node"),
            ("code-server-test/bin/code-server", b"cli"),
        ]);
        let digest = format!("{:x}", Sha256::digest(&first));
        let runtime =
            unpack_runtime(&root, &first, &digest, "code-server-test", "// launcher").unwrap();
        assert_eq!(
            fs::read(runtime.join("launcher.mjs")).unwrap(),
            b"// launcher"
        );
        assert_eq!(
            unpack_runtime(&root, &[], &digest, "code-server-test", "ignored").unwrap(),
            runtime
        );
        assert!(unpack_runtime(&root, &first, &"0".repeat(64), "code-server-test", "").is_err());

        let second = archive(&[("code-server-test/lib/node", b"new node")]);
        let next_digest = format!("{:x}", Sha256::digest(&second));
        let next =
            unpack_runtime(&root, &second, &next_digest, "code-server-test", "// next").unwrap();
        assert_ne!(runtime, next);
        assert!(!runtime.exists());
        assert_eq!(
            fs::read(next.join("code-server-test/lib/node")).unwrap(),
            b"new node"
        );
        let _ = fs::remove_dir_all(root);
    }
}
