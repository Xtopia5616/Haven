use super::*;
use std::{
    fs, io,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "haven-mcp-process-tree-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated MCP process directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn powershell_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn encode_powershell(script: &str) -> String {
    use base64::Engine;
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn exclusive_file_is_locked(path: &Path) -> io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(path)
    {
        Ok(file) => {
            drop(file);
            Ok(false)
        }
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                || matches!(error.raw_os_error(), Some(5 | 32)) =>
        {
            // ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION mean the child
            // still holds its exclusive test file handle.
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

async fn wait_for_lock_state(path: &Path, locked: bool, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if exclusive_file_is_locked(path)? == locked {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn dropping_stdio_transport_terminates_its_process_tree() {
    let temp = TestDirectory::new();
    let parent_lock = temp.path().join("mcp-parent.lock");
    let descendant_lock = temp.path().join("mcp-descendant.lock");
    let stop_file = temp.path().join("stop-test-processes");
    let child_script = format!(
        "$hold = [System.IO.File]::Open('{}', [System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None); while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 50 }}; $hold.Dispose()",
        powershell_literal(&descendant_lock),
        powershell_literal(&stop_file),
    );
    let child_encoded = encode_powershell(&child_script);
    let parent_script = format!(
        "$hold = [System.IO.File]::Open('{}', [System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None); $child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile -NonInteractive -EncodedCommand {child_encoded}' -PassThru; while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 50 }}; $hold.Dispose()",
        powershell_literal(&parent_lock),
        powershell_literal(&stop_file),
    );
    let config = haven_common::config::McpServerConfig {
        name: "stdio-process-containment-test".into(),
        transport: McpServerTransport::Stdio,
        command: "powershell.exe".into(),
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            encode_powershell(&parent_script),
        ],
        cwd: Some(temp.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    let client = McpClient::new(&config, 1024, 1024);
    let (notification_tx, _notification_rx) =
        tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
    let stdio = client
        .spawn_stdio(notification_tx)
        .await
        .expect("spawn MCP stdio adapter");
    let locks_ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if exclusive_file_is_locked(&parent_lock)?
                && exclusive_file_is_locked(&descendant_lock)?
            {
                return Ok::<_, io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok_and(|result| result.is_ok());

    drop(stdio);
    let parent_released = wait_for_lock_state(&parent_lock, false, Duration::from_secs(5))
        .await
        .expect("inspect parent process lock");
    let descendant_released = wait_for_lock_state(&descendant_lock, false, Duration::from_secs(5))
        .await
        .expect("inspect descendant process lock");
    let terminated_by_containment = parent_released && descendant_released;
    if !terminated_by_containment {
        fs::write(&stop_file, b"stop").expect("release test processes after assertion failure");
        let _ = wait_for_lock_state(&parent_lock, false, Duration::from_secs(5)).await;
        let _ = wait_for_lock_state(&descendant_lock, false, Duration::from_secs(5)).await;
    }

    assert!(locks_ready, "MCP stdio server must start its descendant");
    assert!(
        parent_released,
        "dropping StdioInner must terminate its direct server process"
    );
    assert!(
        descendant_released,
        "dropping StdioInner must close the Job Object and terminate its descendant"
    );
}

#[tokio::test]
async fn stdio_policy_revocation_terminates_a_live_process_tree() {
    let temp = TestDirectory::new();
    let parent_lock = temp.path().join("mcp-policy-parent.lock");
    let descendant_lock = temp.path().join("mcp-policy-descendant.lock");
    let stop_file = temp.path().join("stop-policy-test-processes");
    let child_script = format!(
        "$hold = [System.IO.File]::Open('{}', [System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None); while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 50 }}; $hold.Dispose()",
        powershell_literal(&descendant_lock),
        powershell_literal(&stop_file),
    );
    let child_encoded = encode_powershell(&child_script);
    let parent_script = format!(
        "$hold = [System.IO.File]::Open('{}', [System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None); $child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile -NonInteractive -EncodedCommand {child_encoded}' -PassThru; while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 50 }}; while ($line = [Console]::In.ReadLine()) {{ $request = $line | ConvertFrom-Json; if ($null -eq $request.id) {{ continue }}; if ($request.method -eq 'server/discover') {{ $result = @{{ resultType = 'complete'; supportedVersions = @('2026-07-28'); capabilities = @{{ tools = @{{}} }}; _meta = @{{ 'io.modelcontextprotocol/serverInfo' = @{{ name = 'policy-revocation-test'; version = '1' }} }} }} }} elseif ($request.method -eq 'tools/list') {{ $result = @{{ tools = @(); ttlMs = 60000; cacheScope = 'private' }} }} else {{ $result = @{{}} }}; $response = @{{ jsonrpc = '2.0'; id = $request.id; result = $result }} | ConvertTo-Json -Depth 12 -Compress; [Console]::Out.WriteLine($response) }}; $hold.Dispose()",
        powershell_literal(&parent_lock),
        powershell_literal(&descendant_lock),
    );
    let config = haven_common::config::McpServerConfig {
        name: "stdio-policy-revocation-test".into(),
        transport: McpServerTransport::Stdio,
        command: "powershell.exe".into(),
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            encode_powershell(&parent_script),
        ],
        cwd: Some(temp.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    let client = std::sync::Arc::new(McpClient::new(&config, 1024, 1024));
    client
        .set_network_policy(haven_common::types::NetworkPolicy::Open)
        .await;
    let manager = crate::McpManager::new();
    manager.add_client(client.clone()).await;
    client
        .connect()
        .await
        .expect("connect live MCP stdio server");

    let locks_ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if exclusive_file_is_locked(&parent_lock)?
                && exclusive_file_is_locked(&descendant_lock)?
            {
                return Ok::<_, io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok_and(|result| result.is_ok());

    let (signal, receiver) = tokio::sync::watch::channel(std::sync::Arc::new(()));
    manager.watch_stdio_policy_changes(receiver).await;
    signal.send_replace(std::sync::Arc::new(()));
    let removed = tokio::time::timeout(Duration::from_secs(5), async {
        while manager.has_client("stdio-policy-revocation-test").await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    let parent_released = wait_for_lock_state(&parent_lock, false, Duration::from_secs(5))
        .await
        .expect("inspect parent process lock after policy revocation");
    let descendant_released = wait_for_lock_state(&descendant_lock, false, Duration::from_secs(5))
        .await
        .expect("inspect descendant process lock after policy revocation");

    if !(locks_ready && removed && parent_released && descendant_released) {
        fs::write(&stop_file, b"stop").expect("release test processes after assertion failure");
        manager.shutdown_all().await;
        let _ = wait_for_lock_state(&parent_lock, false, Duration::from_secs(5)).await;
        let _ = wait_for_lock_state(&descendant_lock, false, Duration::from_secs(5)).await;
    }

    assert!(
        locks_ready,
        "live MCP server and descendant must acquire their locks"
    );
    assert!(removed, "policy change must remove the live stdio client");
    assert!(
        parent_released,
        "policy revocation must terminate the direct MCP server process"
    );
    assert!(
        descendant_released,
        "policy revocation must terminate the MCP process descendants"
    );
    manager.shutdown_all().await;
}
