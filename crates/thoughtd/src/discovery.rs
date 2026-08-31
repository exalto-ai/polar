//! How clients find the daemon (AD-10).

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Duration;

/// Discovery and local sync protocol understood by this daemon build.
///
/// Version 10 restores monotonic numbering after the protocol-9 preview build
/// and adds a per-instance identity to the simplified discovery contract.
pub const PROTOCOL_VERSION: u32 = 10;
pub const IDENTITY_PATH: &str = "/health/identity";
pub const MCP_HEALTH_PATH: &str = "/health/mcp";
const HEALTH_SERVICE: &str = "ai.exalto.thoughtd";
const MAX_DISCOVERY_BYTES: u64 = 64 * 1024;

/// Exact public response used to confirm that a discovery record still points
/// at the daemon instance that published it. This carries no authority.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityResponse {
    service: String,
    protocol_version: u32,
    instance_id: String,
}

impl IdentityResponse {
    pub fn current(instance_id: &str) -> Self {
        Self {
            service: HEALTH_SERVICE.into(),
            protocol_version: PROTOCOL_VERSION,
            instance_id: instance_id.into(),
        }
    }
}

/// Exact authenticated response used to confirm that the published bearer is
/// accepted for MCP access after the public instance check succeeds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResponse {
    service: String,
    protocol_version: u32,
    instance_id: String,
    capability: String,
}

impl HealthResponse {
    pub fn mcp(instance_id: &str) -> Self {
        Self {
            service: HEALTH_SERVICE.into(),
            protocol_version: PROTOCOL_VERSION,
            instance_id: instance_id.into(),
            capability: "mcp".into(),
        }
    }
}

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

/// What a trusted local client needs to reach a running daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    pub url: String,
    pub protocol_version: u32,
    pub instance_id: String,
    pub token: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentPublishedDaemon {
    protocol_version: u32,
    instance_id: String,
    port: u16,
    token: String,
    url: String,
    store: String,
    pid: u32,
}

/// Non-secret process metadata retained across known discovery upgrades.
///
/// These fields are never sufficient to signal a process. Callers must also
/// verify the live identity response, listener ownership, expected executable,
/// and unchanged discovery bytes immediately before graceful retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedDaemon {
    pub url: String,
    pub protocol_version: u32,
    pub pid: u32,
    pub store: PathBuf,
    pub instance_id: Option<String>,
    record_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyIdentityResponse {
    service: String,
    protocol_version: u32,
}

fn loopback_base(url: &str) -> Option<&str> {
    let base = url.strip_suffix("/mcp")?;
    let port = base.strip_prefix("http://127.0.0.1:")?;
    if port.is_empty() || port.contains('/') || port.parse::<u16>().ok()? == 0 {
        return None;
    }
    Some(base)
}

fn health_url_for(url: &str, path: &str) -> Option<String> {
    loopback_base(url).map(|base| format!("{base}{path}"))
}

fn health_url(daemon: &Daemon, path: &str) -> Option<String> {
    health_url_for(&daemon.url, path)
}

fn is_random_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn local_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .max_idle_connections(0)
        .build()
        .into()
}

fn probe_identity(daemon: &Daemon) -> bool {
    let Some(url) = health_url(daemon, IDENTITY_PATH) else {
        return false;
    };
    let Ok(mut response) = local_agent()
        .get(&url)
        .header("Accept", "application/json")
        .call()
    else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };
    serde_json::from_str::<IdentityResponse>(&body)
        .is_ok_and(|actual| actual == IdentityResponse::current(&daemon.instance_id))
}

/// Verify the exact public service and version published by a known prior
/// daemon without sending any capability from its private discovery record.
pub fn published_identity_reachable(daemon: &PublishedDaemon) -> bool {
    let Some(url) = health_url_for(&daemon.url, IDENTITY_PATH) else {
        return false;
    };
    let Ok(mut response) = local_agent()
        .get(&url)
        .header("Accept", "application/json")
        .call()
    else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };

    match daemon.protocol_version {
        // Protocol 1 and the later protocol-2 mainline shape used an
        // instance-bound identity.
        1 | 2 if daemon.instance_id.is_some() => {
            daemon.instance_id.as_deref().is_some_and(|instance_id| {
                serde_json::from_str::<IdentityResponse>(&body).is_ok_and(|actual| {
                    actual.service == HEALTH_SERVICE
                        && actual.protocol_version == daemon.protocol_version
                        && actual.instance_id == instance_id
                })
            })
        }
        // An earlier stacked protocol-2 preview used the same two-field
        // identity as protocols 3 through 9. Record shape disambiguates it.
        2 => serde_json::from_str::<LegacyIdentityResponse>(&body).is_ok_and(|actual| {
            actual.service == HEALTH_SERVICE && actual.protocol_version == daemon.protocol_version
        }),
        // Protocols 3 through 9 shipped the original two-field public identity.
        3..=9 => serde_json::from_str::<LegacyIdentityResponse>(&body).is_ok_and(|actual| {
            actual.service == HEALTH_SERVICE && actual.protocol_version == daemon.protocol_version
        }),
        _ => false,
    }
}

/// Confirm the public daemon instance before sending its bearer, then verify
/// that the bearer authorizes the expected MCP health endpoint.
pub fn authenticated_reachable(daemon: &Daemon) -> bool {
    if !probe_identity(daemon) {
        return false;
    }
    let Some(url) = health_url(daemon, MCP_HEALTH_PATH) else {
        return false;
    };
    let Ok(mut response) = local_agent()
        .get(&url)
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .header("Accept", "application/json")
        .call()
    else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };
    serde_json::from_str::<HealthResponse>(&body)
        .is_ok_and(|actual| actual == HealthResponse::mcp(&daemon.instance_id))
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::{CStr, c_int, c_void};

    unsafe extern "C" {
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffer_size: u32) -> c_int;
    }

    let mut buffer = vec![0_i8; 4096];
    let length = unsafe {
        proc_pidpath(
            c_int::try_from(pid).ok()?,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    let path = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_executable(_: u32) -> Option<PathBuf> {
    None
}

/// Confirm that a PID still executes the exact bundled sidecar path.
pub fn process_is_expected_daemon(pid: u32, expected: &Path) -> bool {
    process_executable(pid)
        .and_then(|path| path.canonicalize().ok())
        .zip(expected.canonicalize().ok())
        .is_some_and(|(actual, expected)| actual == expected)
}

/// Treat a PID as gone only when process-path inspection and signal zero both
/// agree. Permission errors, unsupported inspection, and PID reuse fail closed.
#[cfg(unix)]
pub fn published_process_definitively_absent(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if process_executable(pid.cast_unsigned()).is_some() {
        return false;
    }
    (unsafe { libc::kill(pid, 0) }) == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub fn published_process_definitively_absent(_: u32) -> bool {
    false
}

fn published_loopback_port(url: &str) -> Option<u16> {
    loopback_base(url)?
        .strip_prefix("http://127.0.0.1:")?
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
pub fn process_owns_published_listener(pid: u32, url: &str) -> bool {
    let Some(port) = published_loopback_port(url) else {
        return false;
    };
    let output = Command::new("/usr/sbin/lsof")
        .args([
            "-nP".to_string(),
            "-a".to_string(),
            "-p".to_string(),
            pid.to_string(),
            format!("-iTCP@127.0.0.1:{port}"),
            "-sTCP:LISTEN".to_string(),
            "-Fp".to_string(),
        ])
        .output();
    output.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == format!("p{pid}"))
    })
}

#[cfg(target_os = "linux")]
pub fn process_owns_published_listener(pid: u32, url: &str) -> bool {
    let Some(port) = published_loopback_port(url) else {
        return false;
    };
    let expected_address = format!("0100007F:{port:04X}");
    let Ok(tcp) = std::fs::read_to_string("/proc/net/tcp") else {
        return false;
    };
    let listening_inodes = tcp
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.get(1).copied() == Some(expected_address.as_str())
                && fields.get(3).copied() == Some("0A")
            {
                fields.get(9).copied()
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if listening_inodes.is_empty() {
        return false;
    }
    let Ok(descriptors) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    descriptors.filter_map(Result::ok).any(|descriptor| {
        std::fs::read_link(descriptor.path())
            .ok()
            .and_then(|target| target.to_str().map(str::to_string))
            .is_some_and(|target| {
                listening_inodes
                    .iter()
                    .any(|inode| target == format!("socket:[{inode}]"))
            })
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn process_owns_published_listener(_: u32, _: &str) -> bool {
    false
}

fn try_lock_file(path: &Path) -> io::Result<Option<File>> {
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
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Process-lifetime ownership of one `THOUGHT_HOME`.
///
/// Both deployed lock names are held in a fixed order. Later preview builds
/// used `daemon.json.lock`, while mainline protocols 1 and 2 used `daemon.lock`.
/// Earlier previews relied on the store lock, which still coordinates the
/// default-store upgrade path. Holding all three keeps known upgrades
/// single-writer across generations.
/// The files remain after exit; OS lock ownership, not presence, is authority.
pub struct HomeLock {
    #[allow(dead_code)]
    legacy: File,
    #[allow(dead_code)]
    current: File,
}

fn legacy_home_lock_path(discovery: &Path) -> PathBuf {
    let mut path = OsString::from(discovery.as_os_str());
    path.push(".lock");
    PathBuf::from(path)
}

fn current_home_lock_path(discovery: &Path) -> io::Result<PathBuf> {
    discovery
        .parent()
        .map(|parent| parent.join("daemon.lock"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "discovery has no parent"))
}

fn try_lock_home_at(discovery: &Path) -> io::Result<Option<HomeLock>> {
    let Some(legacy) = try_lock_file(&legacy_home_lock_path(discovery))? else {
        return Ok(None);
    };
    let Some(current) = try_lock_file(&current_home_lock_path(discovery)?)? else {
        return Ok(None);
    };
    Ok(Some(HomeLock { legacy, current }))
}

pub fn try_lock_home() -> io::Result<Option<HomeLock>> {
    try_lock_home_at(&discovery_path())
}

/// Process-lifetime ownership of the SQLite store.
///
/// This second lock prevents two homes from addressing the same custom store.
/// It is acquired before `Workspace::open`, so competing processes cannot
/// briefly become concurrent CRDT authorities before discovery is published.
pub struct StoreLock(#[allow(dead_code)] File);

fn store_lock_path(db_path: &Path) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(".lock");
    PathBuf::from(path)
}

pub fn try_lock_store(db_path: &Path) -> io::Result<Option<StoreLock>> {
    try_lock_file(&store_lock_path(db_path)).map(|lock| lock.map(StoreLock))
}

fn read_bounded_discovery_at(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let initial_length = file.metadata().ok()?.len();
    let mut body =
        String::with_capacity(usize::try_from(initial_length.min(MAX_DISCOVERY_BYTES)).ok()?);
    (&mut file)
        .take(MAX_DISCOVERY_BYTES + 1)
        .read_to_string(&mut body)
        .ok()?;
    (body.len() as u64 <= MAX_DISCOVERY_BYTES).then_some(body)
}

fn parse_published(body: &str) -> Option<PublishedDaemon> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let protocol_version = u32::try_from(json.get("protocol_version")?.as_u64()?).ok()?;
    let pid = u32::try_from(json.get("pid")?.as_u64()?).ok()?;
    let port = u16::try_from(json.get("port")?.as_u64()?).ok()?;
    let url = json.get("url")?.as_str()?.to_string();
    let store = PathBuf::from(json.get("store")?.as_str()?);
    let instance_id = json
        .get("instance_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    if protocol_version == 0
        || pid == 0
        || port == 0
        || published_loopback_port(&url)? != port
        || store.as_os_str().is_empty()
        || instance_id
            .as_deref()
            .is_some_and(|value| !is_random_id(value))
        || matches!(protocol_version, 1 | PROTOCOL_VERSION) && instance_id.is_none()
    {
        return None;
    }
    Some(PublishedDaemon {
        url,
        protocol_version,
        pid,
        store,
        instance_id,
        record_sha256: Sha256::digest(body.as_bytes()).into(),
    })
}

/// Read the current strict discovery format.
pub fn read() -> Option<Daemon> {
    let body = read_bounded_discovery_at(&discovery_path())?;
    let published: CurrentPublishedDaemon = serde_json::from_str(&body).ok()?;
    if published.protocol_version != PROTOCOL_VERSION
        || published.pid == 0
        || !is_random_id(&published.instance_id)
        || !is_random_id(&published.token)
        || loopback_base(&published.url)?
            .strip_prefix("http://127.0.0.1:")?
            .parse::<u16>()
            .ok()?
            != published.port
    {
        return None;
    }
    Some(Daemon {
        url: published.url,
        protocol_version: published.protocol_version,
        instance_id: published.instance_id,
        token: published.token,
    })
}

/// Read only the non-secret metadata needed to assess a known predecessor.
/// Extra capability fields are intentionally ignored and are never returned.
pub fn read_published() -> Option<PublishedDaemon> {
    read_published_at(&discovery_path())
}

/// Path-specific form used by isolated upgrade tests.
pub fn read_published_at(path: &Path) -> Option<PublishedDaemon> {
    let body = read_bounded_discovery_at(path)?;
    parse_published(&body)
}

/// Confirm that the bounded discovery record still has the exact bytes that
/// produced this metadata. The digest prevents secrets from crossing the API.
pub fn published_record_unchanged(published: &PublishedDaemon) -> bool {
    published_record_unchanged_at(&discovery_path(), published)
}

/// Path-specific form used by the verified handoff and its isolated tests.
pub fn published_record_unchanged_at(path: &Path, published: &PublishedDaemon) -> bool {
    read_bounded_discovery_at(path)
        .and_then(|body| parse_published(&body))
        .as_ref()
        == Some(published)
}

fn known_protocol(protocol_version: u32) -> bool {
    (1..=PROTOCOL_VERSION).contains(&protocol_version)
}

/// Remove a known discovery row only when its process is conclusively gone
/// and both deployed home locks plus its store lock can be held through the
/// final unchanged-bytes and PID checks.
pub fn remove_definitively_stale_discovery() -> io::Result<bool> {
    remove_definitively_stale_discovery_at(&discovery_path(), &default_db_path())
}

fn remove_definitively_stale_discovery_at(path: &Path, expected_store: &Path) -> io::Result<bool> {
    let Some(published) = read_published_at(path) else {
        return Ok(false);
    };
    if published.store != expected_store {
        return Ok(false);
    }
    remove_published_if_definitively_stale_at(path, &published)
}

/// Remove this exact known record only while both home locks and its store lock
/// are held and its PID is conclusively absent. This closes the final race with
/// another app launch publishing a fresh daemon between comparison and unlink.
pub fn remove_published_if_definitively_stale_at(
    path: &Path,
    published: &PublishedDaemon,
) -> io::Result<bool> {
    if !known_protocol(published.protocol_version)
        || !published_process_definitively_absent(published.pid)
    {
        return Ok(false);
    }

    let Some(_home_lock) = try_lock_home_at(path)? else {
        return Ok(false);
    };
    if !published.store.try_exists()? {
        return Ok(false);
    }
    let Some(_store_lock) = try_lock_store(&published.store)? else {
        return Ok(false);
    };
    if thought_store::inspect_compatibility(&published.store)
        .map_err(|error| io::Error::other(format!("could not inspect thought store: {error}")))?
        != thought_store::StoreCompatibility::Current
        || !published.store.try_exists()?
    {
        return Ok(false);
    }
    let Some(current) = read_bounded_discovery_at(path) else {
        return Ok(false);
    };
    if parse_published(&current).as_ref() != Some(published)
        || !published_process_definitively_absent(published.pid)
    {
        return Ok(false);
    }

    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

/// Publish the live instance atomically and owner-readably.
///
/// The daemon holds the home lock while replacing this file, so there is one
/// publisher. A random, create-new temporary path ensures bearer bytes never
/// pass through a pre-existing broad-permission file.
pub fn write(port: u16, token: &str, instance_id: &str, db_path: &Path) -> io::Result<PathBuf> {
    let path = discovery_path();
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "discovery has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(&CurrentPublishedDaemon {
        protocol_version: PROTOCOL_VERSION,
        instance_id: instance_id.into(),
        port,
        token: token.into(),
        url: format!("http://127.0.0.1:{port}/mcp"),
        store: db_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
    })?;
    let temporary = parent.join(format!(".daemon-{instance_id}.tmp"));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&body)?;
        file.sync_all()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        }

        std::fs::rename(&temporary, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::path::Path;

    const LOCK_CHILD: &str = "THOUGHTD_LOCK_CHILD";
    const LOCK_PATH: &str = "THOUGHTD_LOCK_PATH";
    const HOME_LOCK_CHILD: &str = "THOUGHTD_HOME_LOCK_CHILD";
    const HOME_DISCOVERY_PATH: &str = "THOUGHTD_HOME_DISCOVERY_PATH";

    #[test]
    fn lock_child() {
        let Ok(expectation) = std::env::var(LOCK_CHILD) else {
            return;
        };
        let lock_path = std::env::var(LOCK_PATH).expect("child lock path");
        let lock = super::try_lock_file(Path::new(&lock_path)).expect("child lock attempt");
        match expectation.as_str() {
            "blocked" => assert!(lock.is_none(), "the owning process keeps the lock"),
            "available" => assert!(lock.is_some(), "process exit releases the lock"),
            unexpected => panic!("unexpected child lock expectation: {unexpected}"),
        }
    }

    fn run_lock_child(lock_path: &Path, expectation: &str) {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "discovery::tests::lock_child", "--nocapture"])
            .env(LOCK_CHILD, expectation)
            .env(LOCK_PATH, lock_path)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "child lock check failed: {}",
            String::from_utf8_lossy(&child.stderr),
        );
    }

    #[test]
    fn home_lock_child() {
        let Ok(expectation) = std::env::var(HOME_LOCK_CHILD) else {
            return;
        };
        let discovery = std::env::var(HOME_DISCOVERY_PATH).expect("child discovery path");
        let lock = super::try_lock_home_at(Path::new(&discovery)).expect("child home lock attempt");
        match expectation.as_str() {
            "blocked" => assert!(lock.is_none(), "a deployed home lock blocks the child"),
            "available" => assert!(lock.is_some(), "both home locks are available"),
            unexpected => panic!("unexpected child home-lock expectation: {unexpected}"),
        }
    }

    fn run_home_lock_child(discovery: &Path, expectation: &str) {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "discovery::tests::home_lock_child",
                "--nocapture",
            ])
            .env(HOME_LOCK_CHILD, expectation)
            .env(HOME_DISCOVERY_PATH, discovery)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "child home-lock check failed: {}",
            String::from_utf8_lossy(&child.stderr),
        );
    }

    fn published_body(protocol_version: u32, pid: u32, port: u16, store: &Path) -> String {
        let mut body = serde_json::json!({
            "protocol_version": protocol_version,
            "pid": pid,
            "port": port,
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "store": store,
            "token": "secret-mcp-sentinel",
            "editor_token": "secret-editor-sentinel",
            "provider_token": "secret-provider-sentinel"
        });
        if matches!(protocol_version, 1 | 2 | super::PROTOCOL_VERSION) {
            body["instance_id"] = serde_json::Value::String("a".repeat(64));
        }
        body.to_string()
    }

    fn identity_probe(
        response_body: &str,
        published_version: u32,
        instance_bound: bool,
    ) -> (bool, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let store = std::env::temp_dir().join("thought-identity-probe.db");
        let mut published_body: serde_json::Value = serde_json::from_str(&published_body(
            published_version,
            std::process::id(),
            address.port(),
            &store,
        ))
        .unwrap();
        if !instance_bound {
            published_body
                .as_object_mut()
                .unwrap()
                .remove("instance_id");
        }
        let published = super::parse_published(&published_body.to_string()).unwrap();
        let response_body = response_body.to_string();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return String::new();
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("identity server failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = match stream.read(&mut chunk) {
                    Ok(read) => read,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("identity request failed: {error}"),
                };
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            )
            .unwrap();
            String::from_utf8(request).unwrap()
        });
        let reachable = super::published_identity_reachable(&published);
        (reachable, server.join().unwrap())
    }

    #[cfg(unix)]
    fn exited_child_pid() -> u32 {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        assert!(super::published_process_definitively_absent(pid));
        pid
    }

    #[cfg(unix)]
    fn create_version_six_store(path: &Path) {
        drop(thought_store::Store::open(path).unwrap());
        let mut database = std::fs::read(path).unwrap();
        assert!(database.starts_with(b"SQLite format 3\0"));
        database[60..64].copy_from_slice(&6_u32.to_be_bytes());
        std::fs::write(path, database).unwrap();
        assert_eq!(
            thought_store::inspect_compatibility(path).unwrap(),
            thought_store::StoreCompatibility::Unsupported
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
    fn process_lock_is_exclusive_and_released_on_exit() {
        let directory = tempfile::tempdir().unwrap();
        let lock_path = directory.path().join("daemon.lock");
        let first = super::try_lock_file(&lock_path)
            .unwrap()
            .expect("first process owns the lock");
        run_lock_child(&lock_path, "blocked");
        drop(first);
        run_lock_child(&lock_path, "available");
    }

    #[test]
    fn home_lock_coordinates_both_deployed_lock_names() {
        let directory = tempfile::tempdir().unwrap();
        let discovery = directory.path().join("daemon.json");
        let legacy_path = super::legacy_home_lock_path(&discovery);
        let current_path = super::current_home_lock_path(&discovery).unwrap();

        let home = super::try_lock_home_at(&discovery)
            .unwrap()
            .expect("first process owns both home locks");
        run_lock_child(&legacy_path, "blocked");
        run_lock_child(&current_path, "blocked");
        drop(home);
        run_home_lock_child(&discovery, "available");

        let legacy = super::try_lock_file(&legacy_path)
            .unwrap()
            .expect("legacy lock available");
        run_home_lock_child(&discovery, "blocked");
        drop(legacy);
        let current = super::try_lock_file(&current_path)
            .unwrap()
            .expect("current lock available");
        run_home_lock_child(&discovery, "blocked");
        drop(current);
    }

    #[test]
    fn protocol_nine_exposes_only_non_secret_metadata() {
        let store = Path::new("/tmp/thought-preview.db");
        let body = published_body(9, 4321, 4567, store);
        let published = super::parse_published(&body).unwrap();

        assert_eq!(published.protocol_version, 9);
        assert_eq!(published.pid, 4321);
        assert_eq!(published.url, "http://127.0.0.1:4567/mcp");
        assert_eq!(published.store, store);
        assert_eq!(published.instance_id, None);
        let debug = format!("{published:?}");
        assert!(!debug.contains("secret-mcp-sentinel"));
        assert!(!debug.contains("secret-editor-sentinel"));
        assert!(!debug.contains("secret-provider-sentinel"));
    }

    #[test]
    fn oversized_discovery_is_rejected_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.json");
        std::fs::write(&path, vec![b'x'; super::MAX_DISCOVERY_BYTES as usize + 1]).unwrap();

        assert!(super::read_bounded_discovery_at(&path).is_none());
        assert!(super::read_published_at(&path).is_none());
    }

    #[test]
    fn prior_identity_probe_is_exact_and_never_sends_authorization() {
        let body = serde_json::json!({
            "service": "ai.exalto.thoughtd",
            "protocol_version": 9
        })
        .to_string();
        let (reachable, request) = identity_probe(&body, 9, false);
        assert!(reachable);
        assert!(request.starts_with("GET /health/identity HTTP/1.1"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));

        let wrong_service = serde_json::json!({
            "service": "not-thoughtd",
            "protocol_version": 9
        })
        .to_string();
        assert!(!identity_probe(&wrong_service, 9, false).0);

        let wrong_version = serde_json::json!({
            "service": "ai.exalto.thoughtd",
            "protocol_version": 8
        })
        .to_string();
        assert!(!identity_probe(&wrong_version, 9, false).0);
    }

    #[test]
    fn protocols_one_and_two_require_their_exact_identity_shape() {
        let instance_id = "a".repeat(64);
        let protocol_one = serde_json::json!({
            "service": "ai.exalto.thoughtd",
            "protocol_version": 1,
            "instance_id": instance_id
        })
        .to_string();
        assert!(identity_probe(&protocol_one, 1, true).0);

        let protocol_two_bound = serde_json::json!({
            "service": "ai.exalto.thoughtd",
            "protocol_version": 2,
            "instance_id": "a".repeat(64)
        })
        .to_string();
        let protocol_two_legacy = serde_json::json!({
            "service": "ai.exalto.thoughtd",
            "protocol_version": 2
        })
        .to_string();
        assert!(identity_probe(&protocol_two_bound, 2, true).0);
        assert!(identity_probe(&protocol_two_legacy, 2, false).0);
        assert!(!identity_probe(&protocol_two_legacy, 2, true).0);
        assert!(!identity_probe(&protocol_two_bound, 2, false).0);
    }

    #[cfg(unix)]
    #[test]
    fn dead_known_discovery_is_removed_but_unknown_protocol_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let discovery = directory.path().join("daemon.json");
        let store = directory.path().join("thought.db");
        drop(thought_store::Store::open(&store).unwrap());
        let dead_pid = exited_child_pid();

        for protocol_version in [1, 2, 9, super::PROTOCOL_VERSION] {
            std::fs::write(
                &discovery,
                published_body(protocol_version, dead_pid, 4567, &store),
            )
            .unwrap();
            assert!(super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
            assert!(!discovery.exists());
        }

        let unknown = published_body(super::PROTOCOL_VERSION + 1, dead_pid, 4567, &store);
        std::fs::write(&discovery, &unknown).unwrap();
        assert!(!super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
        assert_eq!(std::fs::read_to_string(&discovery).unwrap(), unknown);
    }

    #[cfg(unix)]
    #[test]
    fn stale_cleanup_preserves_discovery_for_missing_and_unsupported_store() {
        let directory = tempfile::tempdir().unwrap();
        let discovery = directory.path().join("daemon.json");
        let store = directory.path().join("thought.db");
        let dead_pid = exited_child_pid();
        let published = published_body(9, dead_pid, 4567, &store);

        std::fs::write(&discovery, &published).unwrap();
        assert!(!super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
        assert!(!store.exists());
        assert_eq!(std::fs::read_to_string(&discovery).unwrap(), published);

        create_version_six_store(&store);
        let store_before = std::fs::read(&store).unwrap();
        assert!(!super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
        assert_eq!(std::fs::read_to_string(&discovery).unwrap(), published);
        assert_eq!(std::fs::read(&store).unwrap(), store_before);
    }

    #[cfg(unix)]
    #[test]
    fn stale_cleanup_refuses_live_process_and_each_busy_lock() {
        let directory = tempfile::tempdir().unwrap();
        let discovery = directory.path().join("daemon.json");
        let store = directory.path().join("thought.db");
        drop(thought_store::Store::open(&store).unwrap());
        let live = published_body(9, std::process::id(), 4567, &store);
        std::fs::write(&discovery, &live).unwrap();
        assert!(!super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
        assert_eq!(std::fs::read_to_string(&discovery).unwrap(), live);

        let dead = published_body(9, exited_child_pid(), 4567, &store);
        std::fs::write(&discovery, &dead).unwrap();
        let legacy = super::try_lock_file(&super::legacy_home_lock_path(&discovery))
            .unwrap()
            .expect("legacy lock available");
        assert!(!super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
        drop(legacy);

        let current = super::try_lock_file(&super::current_home_lock_path(&discovery).unwrap())
            .unwrap()
            .expect("current lock available");
        assert!(!super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
        drop(current);

        let store_lock = super::try_lock_store(&store)
            .unwrap()
            .expect("store lock available");
        assert!(!super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
        drop(store_lock);

        assert_eq!(std::fs::read_to_string(&discovery).unwrap(), dead);
        assert!(super::remove_definitively_stale_discovery_at(&discovery, &store).unwrap());
    }
}
