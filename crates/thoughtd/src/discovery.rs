//! How clients find the daemon (AD-10).

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `THOUGHT_HOME` overrides the location of the store and the discovery file.
/// Without it, a test run would publish itself as *the* daemon and overwrite
/// the real one's port and token.
/// Where the store, the discovery file and the logs live.
pub fn home() -> PathBuf {
    support_dir()
}

fn support_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("THOUGHT_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    #[cfg(target_os = "macos")]
    return Path::new(&home).join("Library/Application Support/ai.exalto.thought");
    // Everywhere else, the XDG data directory. A macOS-shaped path under a
    // Linux `$HOME` would work and would still be the wrong place to look.
    #[cfg(not(target_os = "macos"))]
    return std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(&home).join(".local/share"))
        .join("thought");
}

pub fn default_db_path() -> PathBuf {
    support_dir().join("thought.db")
}

pub fn discovery_path() -> PathBuf {
    support_dir().join("daemon.json")
}

/// 256 bits from the OS. Not a dependency worth taking for this.
///
/// `read_exact`, never `fs::read`: `/dev/urandom` has no EOF, so reading it to
/// the end hangs forever while consuming memory. The daemon did exactly that
/// before it could log a single line.
pub fn random_token() -> io::Result<String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// What a client needs to reach a running daemon.
#[derive(Debug, Clone)]
pub struct Daemon {
    pub url: String,
    pub token: String,
}

fn probe_status(daemon: &Daemon, token: &str) -> Option<u16> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .max_idle_connections(0)
        .build()
        .into();
    let response = agent
        .post(&daemon.url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/json, text/event-stream")
        .send_json(serde_json::json!({
            "jsonrpc": "2.0", "id": 0, "method": "ping"
        }));
    match response {
        Ok(response) => Some(response.status().as_u16()),
        Err(ureq::Error::StatusCode(status)) => Some(status),
        Err(_) => None,
    }
}

/// Confirm that the published endpoint accepts the bearer token in discovery.
///
/// An MCP `ping` may return an application-level error before a session is
/// initialized, so any HTTP answer other than the auth layer's 401 is proof
/// that the request reached the authenticated daemon. The one-second timeout
/// keeps both native and stdio startup bounded.
pub fn authenticated_reachable(daemon: &Daemon) -> bool {
    probe_status(daemon, &daemon.token).is_some_and(|status| status != 401)
}

/// A process-lifetime advisory lock for the SQLite store.
///
/// The lock is acquired before `Workspace::open`, so racing app and stdio
/// launches cannot briefly become concurrent writers while discovery is being
/// published. The lock file remains on disk; closing this handle releases the
/// operating-system lock.
pub struct StoreLock(#[allow(dead_code)] File);

fn store_lock_path(db_path: &Path) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(".lock");
    PathBuf::from(path)
}

pub fn try_lock_store(db_path: &Path) -> io::Result<Option<StoreLock>> {
    let path = store_lock_path(db_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(StoreLock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Read the published daemon, if one has published itself.
///
/// Presence of the file is not proof of life: a daemon killed with SIGKILL
/// leaves it behind, and its port may since have been reused by an unrelated
/// process. The authenticated reachability check is the caller's job.
pub fn read() -> Option<Daemon> {
    let body = std::fs::read_to_string(discovery_path()).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some(Daemon {
        url: json.get("url")?.as_str()?.to_string(),
        token: json.get("token")?.as_str()?.to_string(),
    })
}

/// Publish the port and token, readable only by the user.
pub fn write(port: u16, token: &str, db_path: &Path) -> io::Result<PathBuf> {
    let path = discovery_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "port": port,
        "token": token,
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "store": db_path.to_string_lossy(),
        // Diagnostic metadata and integration-test cleanup only. Clients do
        // not signal this PID or use it to decide whether to start a daemon.
        "pid": std::process::id(),
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The token is a bearer credential for the user's private writing.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    const STORE_LOCK_CHILD: &str = "THOUGHTD_STORE_LOCK_CHILD";
    const STORE_LOCK_DB_PATH: &str = "THOUGHTD_STORE_LOCK_DB_PATH";

    #[test]
    fn store_lock_child() {
        let Ok(expectation) = std::env::var(STORE_LOCK_CHILD) else {
            return;
        };
        let db_path = std::env::var(STORE_LOCK_DB_PATH).expect("child store path");
        let lock = super::try_lock_store(Path::new(&db_path)).expect("child lock attempt");
        match expectation.as_str() {
            "blocked" => assert!(lock.is_none(), "the owning process keeps the lock"),
            "available" => assert!(lock.is_some(), "process exit releases the lock"),
            unexpected => panic!("unexpected child lock expectation: {unexpected}"),
        }
    }

    fn run_store_lock_child(db_path: &Path, expectation: &str) {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "discovery::tests::store_lock_child",
                "--nocapture",
            ])
            .env(STORE_LOCK_CHILD, expectation)
            .env(STORE_LOCK_DB_PATH, db_path)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "child lock check failed: {}",
            String::from_utf8_lossy(&child.stderr),
        );
    }

    #[test]
    fn token_is_256_bits_of_hex_and_returns_promptly() {
        let token = super::random_token().expect("urandom is readable");
        assert_eq!(token.len(), 64, "32 bytes, hex-encoded");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(
            token,
            super::random_token().unwrap(),
            "must not be constant"
        );
    }

    #[test]
    fn only_one_process_can_lock_a_store() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("thought.db");
        let first = super::try_lock_store(&db_path)
            .unwrap()
            .expect("first process owns the store");
        run_store_lock_child(&db_path, "blocked");
        drop(first);
        run_store_lock_child(&db_path, "available");
    }
}
