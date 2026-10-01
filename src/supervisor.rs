//! Supervises the official code-server release next to the plugin host.
//!
//! The release archive is embedded at package-build time, verified again before
//! extraction, and unpacked once per checksum. User settings/extensions live in
//! `data/`, separately from the replaceable runtime. The launcher and code-server
//! never inherit the plugin host's IPC stdin/stdout or its private environment.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const PLUGIN_DIR: &str = "code_server";

#[derive(Debug, Clone, PartialEq)]
pub struct Configuration {
    pub workspace_root: Option<PathBuf>,
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
        Ok(Self { workspace_root })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Unconfigured,
    Starting,
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
            Status::Unconfigured | Status::Starting => json!({"status": "starting"}),
        }
    }
}

struct Process {
    child: Child,
    stdin: ChildStdin,
}

pub struct Supervisor {
    archive: &'static [u8],
    checksum: &'static str,
    version: &'static str,
    server_root: &'static str,
    launcher: &'static str,
    status: Mutex<Status>,
    process: Mutex<Option<Process>>,
    configuration: Mutex<Option<Configuration>>,
    runtime_lock: Mutex<()>,
    generation: AtomicU64,
}

impl Supervisor {
    pub fn new(
        archive: &'static [u8],
        checksum: &'static str,
        version: &'static str,
        server_root: &'static str,
        launcher: &'static str,
    ) -> Arc<Self> {
        Arc::new(Self {
            archive,
            checksum,
            version,
            server_root,
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

    /// Starts/restarts after POM configuration changes; startup never blocks IPC.
    pub fn configure(self: &Arc<Self>, configuration: Configuration) {
        let generation = {
            let Ok(mut current) = self.configuration.lock() else {
                return;
            };
            if current.as_ref() == Some(&configuration)
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
        if self.archive.is_empty() {
            return Err("this build does not bundle the code-server runtime".into());
        }
        if self.server_root.is_empty() || self.version.is_empty() {
            return Err("this build has incomplete code-server metadata".into());
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
            unpack_runtime(
                &base.join("runtime"),
                self.archive,
                self.checksum,
                self.server_root,
                self.launcher,
            )?
        };
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        let code_root = runtime.join(self.server_root);
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
            .env("POM_CODE_SERVER_VERSION", self.version)
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
        let detail = json!({"version": self.version});
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

/// Verify and atomically unpack the official release once per content digest.
pub fn unpack_runtime(
    root: &Path,
    archive: &[u8],
    checksum: &str,
    server_root: &str,
    launcher: &str,
) -> Result<PathBuf, String> {
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("code-server archive checksum is invalid".into());
    }
    if server_root.is_empty() || Path::new(server_root).components().count() != 1 {
        return Err("code-server release directory is invalid".into());
    }
    let id = &checksum[..16];
    let target = root.join(id);
    if target.join(".complete").is_file()
        && target.join(server_root).join("lib").is_dir()
        && target.join("launcher.mjs").is_file()
    {
        return Ok(target);
    }
    let actual = format!("{:x}", Sha256::digest(archive));
    if !actual.eq_ignore_ascii_case(checksum) {
        return Err("code-server archive checksum mismatch".into());
    }
    fs::create_dir_all(root).map_err(|error| format!("{}: {error}", root.display()))?;
    let partial = root.join(format!(".{id}.partial"));
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    let mut unpacker = tar::Archive::new(flate2::read::GzDecoder::new(archive));
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
            if entry.file_name() != id {
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
        let parsed =
            Configuration::from_configure(&json!({"workspace_root": "/tmp/workspace"})).unwrap();
        assert_eq!(parsed.workspace_root, Some(PathBuf::from("/tmp/workspace")));
        assert!(Configuration::from_configure(&json!({"workspace_root": 42})).is_err());
        assert!(Configuration::from_configure(&json!({"workspace_root": "relative"})).is_err());
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
