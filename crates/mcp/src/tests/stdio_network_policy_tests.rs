use super::*;
use haven_common::types::NetworkPolicy;
use std::path::PathBuf;

const STDIO_PROBE_MARKER: &str = "HAVEN_MCP_STDIO_POLICY_PROBE";

struct ProbeMarker(PathBuf);

impl ProbeMarker {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "haven-mcp-stdio-policy-{}-{stamp}.marker",
            std::process::id()
        )))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for ProbeMarker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn non_open_network_policies_reject_stdio_before_spawning_the_command() {
    let probe = ProbeMarker::new();
    let executable = std::env::current_exe().expect("current test executable");
    let mut probe_command = std::process::Command::new(&executable);
    probe_command
        .args([
            "--exact",
            "tests::stdio_network_policy_tests::stdio_spawn_probe_child",
            "--nocapture",
        ])
        .env(STDIO_PROBE_MARKER, probe.path());
    let probe_child = {
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
        probe_command.spawn().expect("start spawn-probe test child")
    };
    let probe_output = probe_child
        .wait_with_output()
        .expect("collect spawn-probe test child");
    assert!(
        probe_output.status.success(),
        "spawn-probe child failed: {}",
        String::from_utf8_lossy(&probe_output.stderr)
    );
    assert!(probe.path().exists(), "spawn-probe child must write marker");
    std::fs::remove_file(probe.path()).expect("remove spawn-probe marker before policy checks");

    for policy in [
        NetworkPolicy::Ask,
        NetworkPolicy::Restricted,
        NetworkPolicy::Deny,
    ] {
        let marker = ProbeMarker::new();
        let config = McpServerConfig {
            name: format!("stdio-policy-{policy:?}"),
            transport: haven_common::mcp::McpServerTransport::Stdio,
            command: executable.to_string_lossy().into_owned(),
            args: vec![
                "--exact".into(),
                "tests::stdio_network_policy_tests::stdio_spawn_probe_child".into(),
                "--nocapture".into(),
            ],
            env: vec![format!("{STDIO_PROBE_MARKER}={}", marker.path().display())],
            ..Default::default()
        };
        let client = McpClient::new(&config, 1024, 1024);
        client.set_network_policy(policy).await;

        let outcome = tokio::time::timeout(Duration::from_secs(3), client.connect()).await;

        assert!(
            !marker.path().exists(),
            "{policy:?} policy must reject stdio before spawning its configured command"
        );
        assert!(
            matches!(outcome, Ok(Err(_))),
            "{policy:?} policy should synchronously reject stdio, got {outcome:?}"
        );
        assert!(matches!(
            client.status().await,
            McpClientStatus::Offline { .. }
        ));
    }
}

#[tokio::test]
async fn stdio_process_start_without_a_tools_authorization_snapshot_fails_closed() {
    let marker = ProbeMarker::new();
    let executable = std::env::current_exe().expect("current test executable");
    let config = McpServerConfig {
        name: "stdio-missing-process-authorization".into(),
        transport: haven_common::mcp::McpServerTransport::Stdio,
        command: executable.to_string_lossy().into_owned(),
        args: vec![
            "--exact".into(),
            "tests::stdio_network_policy_tests::stdio_spawn_probe_child".into(),
            "--nocapture".into(),
        ],
        env: vec![format!("{STDIO_PROBE_MARKER}={}", marker.path().display())],
        ..Default::default()
    };
    let client = crate::client::McpClient::new_managed(&config, 1024, 1024, None);
    client.set_network_policy(NetworkPolicy::Open).await;

    let result = tokio::time::timeout(Duration::from_secs(3), client.connect()).await;

    assert!(matches!(result, Ok(Err(_))));
    assert!(
        !marker.path().exists(),
        "an MCP stdio child must not spawn without a Tools-owned authorization snapshot"
    );
}

#[test]
fn stdio_spawn_probe_child() {
    let Ok(path) = std::env::var(STDIO_PROBE_MARKER) else {
        return;
    };
    std::fs::write(path, b"spawned").expect("write stdio spawn marker");
}
