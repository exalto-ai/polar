//! Native launcher failure reporting and reviewer-secret regression coverage.

mod harness;

use harness::Daemon;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use thought_credentials::CredentialStore;
use thought_mcp::{ReviewerClient, ReviewerPermissions, Workspace};
use thoughtd::connections::ConnectionRegistry;

struct SpawnedDaemonCleanup(PathBuf);

impl Drop for SpawnedDaemonCleanup {
    fn drop(&mut self) {
        let Ok(body) = std::fs::read_to_string(self.0.join("daemon.json")) else {
            return;
        };
        let Ok(discovery) = serde_json::from_str::<serde_json::Value>(&body) else {
            return;
        };
        if let Some(pid) = discovery["pid"].as_u64() {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
    }
}

fn editor_base(daemon: &Daemon) -> &str {
    daemon.url.strip_suffix("/mcp").unwrap()
}

fn post_json(
    url: &str,
    bearer: &str,
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::post(url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .send(body)
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            collect_files(&entry.path(), files);
        } else if file_type.is_file() {
            files.push(entry.path());
        }
    }
}

fn assert_secrets_absent(label: &str, bytes: &[u8], secrets: &[&str]) {
    for secret in secrets {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "reviewer credential leaked into {label}"
        );
    }
}

fn capture_json_response(
    mut response: ureq::http::Response<ureq::Body>,
    captured: &mut Vec<Vec<u8>>,
) -> serde_json::Value {
    let body = response.body_mut().read_to_string().unwrap();
    captured.push(body.as_bytes().to_vec());
    serde_json::from_str(&body).unwrap()
}

fn prepare_failure_reporter(daemon: &Daemon, instance_id: &str) -> i64 {
    let url = format!(
        "{}/editor/reviewer-connections/{}/failure-reporter",
        editor_base(daemon),
        daemon.connection_id
    );
    let body = serde_json::json!({ "instance_id": instance_id }).to_string();
    let mut response = post_json(&url, &daemon.token, &body).unwrap();
    let response: serde_json::Value = response.body_mut().read_json().unwrap();
    response["credential_version"].as_i64().unwrap()
}

fn failure_body(failure_code: &str, instance_id: &str) -> String {
    serde_json::json!({
        "failure_code": failure_code,
        "instance_id": instance_id,
    })
    .to_string()
}

#[test]
fn only_the_platform_capability_can_report_schema_failures() {
    let daemon = Daemon::start();
    let url = format!(
        "{}/editor/reviewer-connections/{}/failure",
        editor_base(&daemon),
        daemon.connection_id
    );
    let instance_id = "native-test-process";
    prepare_failure_reporter(&daemon, instance_id);
    let body = failure_body("transport", instance_id);

    assert!(matches!(
        post_json(&url, &daemon.reviewer_token, &body),
        Err(ureq::Error::StatusCode(401))
    ));

    let mut response = post_json(&url, &daemon.token, &body).unwrap();
    let reported: serde_json::Value = response.body_mut().read_json().unwrap();
    assert_eq!(reported["connection"]["status"], "failed");
    assert_eq!(reported["connection"]["failure_code"], "transport");
    assert!(!reported.to_string().contains(&daemon.reviewer_token));

    prepare_failure_reporter(&daemon, instance_id);
    assert!(matches!(
        post_json(&url, &daemon.token, &failure_body("arbitrary", instance_id),),
        Err(ureq::Error::StatusCode(400))
    ));
    assert!(matches!(
        post_json(
            &format!(
                "{}/editor/reviewer-connections/reviewer-missing/failure",
                editor_base(&daemon)
            ),
            &daemon.token,
            &body,
        ),
        Err(ureq::Error::StatusCode(404))
    ));
}

#[test]
fn reset_rejects_a_failure_report_from_the_prior_process_generation() {
    let daemon = Daemon::start();
    let instance_id = "pre-reset-process";
    prepare_failure_reporter(&daemon, instance_id);

    let reset = post_json(
        &format!(
            "{}/editor/reviewer-connections/{}/reset",
            editor_base(&daemon),
            daemon.connection_id
        ),
        &daemon.token,
        r#"{"expected_revision":1}"#,
    )
    .unwrap();
    let reset = capture_json_response(reset, &mut Vec::new());
    assert_eq!(reset["connection"]["status"], "disconnected");

    let stale = failure_body("transport", instance_id);
    assert!(matches!(
        post_json(
            &format!(
                "{}/editor/reviewer-connections/{}/failure",
                editor_base(&daemon),
                daemon.connection_id
            ),
            &daemon.token,
            &stale,
        ),
        Err(ureq::Error::StatusCode(409))
    ));

    let mut response = ureq::get(format!(
        "{}/editor/reviewer-connections",
        editor_base(&daemon)
    ))
    .header("Authorization", &format!("Bearer {}", daemon.token))
    .call()
    .unwrap();
    let listed: serde_json::Value = response.body_mut().read_json().unwrap();
    let saved = listed["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|saved| saved["id"] == daemon.connection_id)
        .unwrap();
    assert_eq!(saved["status"], "disconnected");
    assert!(saved["failure_code"].is_null());
}

#[test]
fn old_and_new_reviewer_credentials_stay_out_of_native_artifacts_and_api_bodies() {
    let daemon = Daemon::start();
    daemon.connect();
    let document_id = daemon.create_document("Secret regression provenance");
    let _lineage = daemon.call(
        "document_lineage",
        serde_json::json!({"doc_id": &document_id}),
    );

    let base = editor_base(&daemon);
    let connection = &daemon.connection_id;
    let mut captured_api_bodies = Vec::new();
    let mut captured_api_responses = Vec::new();

    let list_response = ureq::get(format!("{base}/editor/reviewer-connections"))
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .call()
        .unwrap();
    let listed = capture_json_response(list_response, &mut captured_api_responses);
    assert_eq!(listed["connections"][0]["id"], *connection);

    let rename_body = r#"{"expected_revision":1,"display_label":"Secret scan reviewer"}"#;
    captured_api_bodies.push(rename_body.as_bytes().to_vec());
    let rename_response = ureq::patch(format!("{base}/editor/reviewer-connections/{connection}"))
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .header("Content-Type", "application/json")
        .send(rename_body)
        .unwrap();
    let renamed = capture_json_response(rename_response, &mut captured_api_responses);
    assert_eq!(renamed["connection"]["revision"], 2);

    let reporter_body = r#"{"instance_id":"secret-scan-process"}"#;
    captured_api_bodies.push(reporter_body.as_bytes().to_vec());
    let reporter_response = post_json(
        &format!("{base}/editor/reviewer-connections/{connection}/failure-reporter"),
        &daemon.token,
        reporter_body,
    )
    .unwrap();
    let reporter = capture_json_response(reporter_response, &mut captured_api_responses);
    assert!(reporter["credential_version"].as_i64().unwrap() > 0);

    let failure_body = failure_body("protocol", "secret-scan-process");
    captured_api_bodies.push(failure_body.as_bytes().to_vec());
    let failure_response = post_json(
        &format!("{base}/editor/reviewer-connections/{connection}/failure"),
        &daemon.token,
        &failure_body,
    )
    .unwrap();
    let failed = capture_json_response(failure_response, &mut captured_api_responses);
    assert_eq!(failed["connection"]["failure_code"], "protocol");

    let reset_body = r#"{"expected_revision":2}"#;
    captured_api_bodies.push(reset_body.as_bytes().to_vec());
    let reset_response = post_json(
        &format!("{base}/editor/reviewer-connections/{connection}/reset"),
        &daemon.token,
        reset_body,
    )
    .unwrap();
    let reset = capture_json_response(reset_response, &mut captured_api_responses);
    assert_eq!(reset["connection"]["revision"], 3);

    let old_credential = daemon.reviewer_token.as_str();
    let credential_path = daemon
        .home
        .path()
        .join("reviewer-credentials")
        .join(format!("{connection}.credential"));
    let new_credential = std::fs::read_to_string(&credential_path).unwrap();
    assert_ne!(old_credential, new_credential);
    let secrets = [old_credential, new_credential.as_str()];

    let list_after_reset = ureq::get(format!("{base}/editor/reviewer-connections"))
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .call()
        .unwrap();
    let listed_after_reset = capture_json_response(list_after_reset, &mut captured_api_responses);
    assert_eq!(listed_after_reset["connections"][0]["revision"], 3);

    for (index, body) in captured_api_bodies.iter().enumerate() {
        assert_secrets_absent(
            &format!("captured API request body {index}"),
            body,
            &secrets,
        );
    }
    for (index, body) in captured_api_responses.iter().enumerate() {
        assert_secrets_absent(
            &format!("captured API response body {index}"),
            body,
            &secrets,
        );
    }

    let home = daemon.home.path();
    for required in [
        home.join("daemon.json"),
        home.join("thought.db"),
        home.join("thought.db-wal"),
        home.join("thought.db-shm"),
    ] {
        assert!(
            required.exists(),
            "missing regression artifact: {required:?}"
        );
    }
    assert!(
        std::fs::read_dir(home).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("thoughtd")
        }),
        "daemon logs were not present for the regression scan"
    );

    let mut artifacts = Vec::new();
    collect_files(home, &mut artifacts);
    for artifact in artifacts {
        // This is the sole intended native storage location in the file-backed
        // integration environment. Everything else, including SQLite's WAL and
        // SHM, logs, lifecycle rows, and provenance rows, must stay secret-free.
        if artifact == credential_path {
            continue;
        }
        let bytes = std::fs::read(&artifact).unwrap();
        assert_secrets_absent(&artifact.display().to_string(), &bytes, &secrets);
    }
}

#[test]
fn a_missing_native_credential_marks_the_saved_connection_failed() {
    let daemon = Daemon::start();
    let credential_path = daemon
        .home
        .path()
        .join("reviewer-credentials")
        .join(format!("{}.credential", daemon.connection_id));
    std::fs::remove_file(credential_path).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .env("THOUGHT_HOME", daemon.home.path())
        .env("THOUGHT_CREDENTIAL_BACKEND", "file")
        .arg("--connection")
        .arg(&daemon.connection_id)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not load reviewer connection"));

    let mut response = ureq::get(format!(
        "{}/editor/reviewer-connections",
        editor_base(&daemon)
    ))
    .header("Authorization", &format!("Bearer {}", daemon.token))
    .call()
    .unwrap();
    let listed: serde_json::Value = response.body_mut().read_json().unwrap();
    let connection = listed["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connection| connection["id"] == daemon.connection_id)
        .unwrap();
    assert_eq!(connection["status"], "failed");
    assert_eq!(connection["failure_code"], "credential_missing");
}

#[test]
fn a_cold_launch_starts_the_daemon_before_reporting_a_missing_credential() {
    let home = tempfile::tempdir().unwrap();
    let workspace = Arc::new(Workspace::open(home.path().join("thought.db")).unwrap());
    let registry = ConnectionRegistry::new(
        workspace.clone(),
        CredentialStore::files(home.path().join("reviewer-credentials")),
    );
    let connection = registry
        .create(
            ReviewerClient::Codex,
            "Cold launch reviewer".to_string(),
            ReviewerPermissions::all(true, true, true),
            10,
        )
        .unwrap();
    std::fs::remove_file(
        home.path()
            .join("reviewer-credentials")
            .join(format!("{}.credential", connection.id)),
    )
    .unwrap();
    drop(registry);
    drop(workspace);

    let _cleanup = SpawnedDaemonCleanup(home.path().to_path_buf());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .env("THOUGHT_HOME", home.path())
        .env("THOUGHT_CREDENTIAL_BACKEND", "file")
        .arg("--connection")
        .arg(&connection.id)
        .output()
        .unwrap();
    assert!(!output.status.success());

    let discovery: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("daemon.json")).unwrap())
            .unwrap();
    let base = discovery["url"]
        .as_str()
        .unwrap()
        .strip_suffix("/mcp")
        .unwrap();
    let platform_token = discovery["token"].as_str().unwrap();
    let mut response = ureq::get(format!("{base}/editor/reviewer-connections"))
        .header("Authorization", &format!("Bearer {platform_token}"))
        .call()
        .unwrap();
    let listed: serde_json::Value = response.body_mut().read_json().unwrap();
    let saved = listed["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|saved| saved["id"] == connection.id)
        .unwrap();
    assert_eq!(saved["status"], "failed");
    assert_eq!(saved["failure_code"], "credential_missing");
}

#[test]
fn an_old_running_shim_cannot_overwrite_a_deliberate_reset_failure_state() {
    let daemon = Daemon::start();
    let mut shim = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .env("THOUGHT_HOME", daemon.home.path())
        .env("THOUGHT_CREDENTIAL_BACKEND", "file")
        .arg("--connection")
        .arg(&daemon.connection_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = shim.stdin.take().unwrap();
    let mut output = BufReader::new(shim.stdout.take().unwrap());

    writeln!(
        input,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "reset-race", "version": "1"}
            }
        })
    )
    .unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"],
        1
    );

    let reset = post_json(
        &format!(
            "{}/editor/reviewer-connections/{}/reset",
            editor_base(&daemon),
            daemon.connection_id
        ),
        &daemon.token,
        r#"{"expected_revision":1}"#,
    )
    .unwrap();
    let reset = capture_json_response(reset, &mut Vec::new());
    assert_eq!(reset["connection"]["status"], "disconnected");

    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    )
    .unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let denied: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(denied["id"], 2);
    assert!(denied.get("error").is_some(), "{denied}");

    let mut response = ureq::get(format!(
        "{}/editor/reviewer-connections",
        editor_base(&daemon)
    ))
    .header("Authorization", &format!("Bearer {}", daemon.token))
    .call()
    .unwrap();
    let listed: serde_json::Value = response.body_mut().read_json().unwrap();
    let saved = listed["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|saved| saved["id"] == daemon.connection_id)
        .unwrap();
    assert_eq!(saved["status"], "disconnected");
    assert!(saved["failure_code"].is_null());

    drop(input);
    let _ = shim.wait();
}
