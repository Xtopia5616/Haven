//! Windows-only proof that the production AppContainer process owner combines
//! a private workspace, filtered environment, explicit stdio handles, and a
//! default-deny file/network boundary.

use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::appcontainer_process::AppContainerCommand;
use crate::filesystem::{
    DirectoryCapability, DirectoryTreeChangeKind, DirectoryTreeCopyLimits, DirectoryTreeDiffLimits,
};
use crate::process_environment::filter_shell_environment;
use crate::windows_sandbox::AppContainerWorkspace;

const APP_CONTAINER_TEST_TIMEOUT: Duration = Duration::from_secs(25);

#[test]
fn appcontainer_process_is_limited_to_granted_workspace_and_has_no_network_capability() {
    let fixture = tempfile::tempdir().expect("create AppContainer test fixture");
    let source_path = fixture.path().join("source");
    std::fs::create_dir(&source_path).expect("create snapshot source");
    let source_executable = source_path.join("appcontainer-probe.exe");
    std::fs::copy(
        std::env::current_exe().expect("current test executable"),
        &source_executable,
    )
    .expect("copy test executable into snapshot source");
    let outside_file = fixture.path().join("outside-sentinel.txt");
    std::fs::write(&outside_file, "outside-sentinel").expect("create outside sentinel");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local-only network probe");
    listener
        .set_nonblocking(true)
        .expect("make the local probe nonblocking");
    let probe_address = listener.local_addr().expect("read probe address");
    let baseline_connection = TcpStream::connect_timeout(&probe_address, Duration::from_secs(2))
        .expect("host process can reach its local probe listener");
    let _ = listener
        .accept()
        .expect("accept the host process baseline connection");
    drop(baseline_connection);
    let network_probe_port = probe_address.port();

    let source = DirectoryCapability::open(&source_path).expect("open snapshot source capability");
    let limits = DirectoryTreeCopyLimits::new(16, 4, 512 * 1024 * 1024, 1024 * 1024 * 1024)
        .expect("valid bounded snapshot limits");
    let workspace = AppContainerWorkspace::create(&source, 7, 11, limits, || false)
        .expect("create a policy-bound low-integrity AppContainer workspace");
    assert_eq!(workspace.execution_workspace_generation(), 7);
    assert_eq!(workspace.policy_revision(), 11);
    assert_eq!(workspace.snapshot_summary().file_count, 1);

    let probe_executable = workspace.root().join("appcontainer-probe.exe");
    let baseline_root = workspace.baseline_root_for_test().to_path_buf();
    let command = AppContainerCommand::new(&probe_executable)
        .args([
            "--exact",
            "appcontainer_probe_tests::appcontainer_child_probe",
            "--nocapture",
            "--",
            "--probe-child",
            outside_file.to_string_lossy().as_ref(),
            workspace.root().to_string_lossy().as_ref(),
            baseline_root.to_string_lossy().as_ref(),
            &network_probe_port.to_string(),
        ])
        .envs(filter_shell_environment(std::env::vars_os()))
        .env("HAVEN_PROBE_EXPLICIT", "visible");
    let sandbox_root = workspace.root().to_path_buf();
    let mut process = command
        .spawn(workspace)
        .expect("spawn through the production AppContainer process owner");
    assert_eq!(
        process
            .output_manifest(
                DirectoryTreeDiffLimits::new(limits, 32, 16 * 1024 * 1024, 32 * 1024 * 1024)
                    .expect("valid bounded output manifest limits"),
                || false,
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied,
        "output cannot be inspected while a process may still mutate it"
    );
    let review_limits =
        DirectoryTreeDiffLimits::new(limits, 32, 16 * 1024 * 1024, 32 * 1024 * 1024)
            .expect("valid bounded output manifest limits");
    let Err(error) = process.take_output_review(review_limits, || false) else {
        panic!("sandbox output must not be retained while the tree is running");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::PermissionDenied,
        "sandbox output review cannot start while the process tree may still write"
    );
    let exit_status = process
        .wait_timeout(APP_CONTAINER_TEST_TIMEOUT)
        .expect("wait for the AppContainer process")
        .expect("AppContainer probe exits before deadline");
    let mut stdout = String::new();
    process
        .take_stdout()
        .expect("AppContainer stdout is piped")
        .read_to_string(&mut stdout)
        .expect("read AppContainer stdout through the explicit handle list");
    let review = process
        .take_output_review(review_limits, || false)
        .expect("retain and scan output after the AppContainer process tree exits");
    drop(process);
    assert!(
        sandbox_root.exists(),
        "the review owner keeps sandbox output alive after dropping the process handle"
    );
    let created_change = review
        .manifest()
        .changes
        .iter()
        .find(|change| change.relative_path == Path::new("inside-workspace.txt"))
        .expect("probe output manifest includes its created file");
    let promotion_path = fixture.path().join("promoted");
    std::fs::create_dir(&promotion_path).expect("create promotion destination");
    let promotion_destination =
        DirectoryCapability::open(&promotion_path).expect("open authorized promotion destination");
    let promoted = review
        .promote_output_file_to(
            created_change,
            7,
            11,
            &promotion_destination,
            Path::new("inside-workspace.txt"),
            || false,
        )
        .expect("promote the reviewed file after the process tree exits");
    assert_eq!(promoted.bytes, b"inside-ok".len() as u64);
    assert_eq!(
        std::fs::read(promotion_path.join("inside-workspace.txt")).unwrap(),
        b"inside-ok"
    );
    assert_eq!(
        review
            .promote_output_file_to(
                created_change,
                7,
                11,
                &promotion_destination,
                Path::new("inside-workspace.txt"),
                || false,
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists,
        "promotion never overwrites a file that appeared after review"
    );
    assert_eq!(
        review
            .promote_output_file_to(
                created_change,
                8,
                11,
                &promotion_destination,
                Path::new("stale-generation.txt"),
                || false,
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied,
        "promotion rejects a stale execution workspace generation"
    );
    for path in ["inside-workspace.txt", "appcontainer-result.txt"] {
        assert!(
            review.manifest().changes.iter().any(|change| {
                change.relative_path.as_path() == Path::new(path)
                    && change.change == DirectoryTreeChangeKind::Created
                    && change.after_sha256.is_some()
            }),
            "bounded output manifest must identify the sandbox-created file {path}"
        );
    }
    let result_path = sandbox_root.join("appcontainer-result.txt");
    let result = std::fs::read_to_string(&result_path).unwrap_or_default();
    let inside_file_was_written = sandbox_root.join("inside-workspace.txt").is_file();
    let outside_content = std::fs::read_to_string(&outside_file).expect("read outside sentinel");
    let accepted_network_connection = match listener.accept() {
        Ok((_stream, _address)) => true,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("local network probe failed: {error}"),
    };
    review.cleanup().expect("discard retained sandbox review");

    assert!(
        !sandbox_root.exists(),
        "sandbox workspace was removed after tree exit"
    );
    assert_eq!(source.entries(2).unwrap().len(), 1);
    assert!(
        !source_path.join("inside-workspace.txt").exists(),
        "AppContainer writes did not reach the canonical source"
    );

    assert!(
        exit_status.success(),
        "AppContainer probe failed; result={result:?}, stdout={stdout:?}, inside_file={inside_file_was_written}"
    );
    assert!(
        inside_file_was_written,
        "container could not write its granted workspace"
    );
    assert_eq!(outside_content, "outside-sentinel");
    assert_eq!(
        result.trim(),
        "read_denied=true;write_denied=true;baseline_denied=true;network_denied=true"
    );
    assert!(
        stdout.contains("ambient_secret_filtered=true;explicit_env=true"),
        "the process must receive only filtered environment entries through an empty-by-default block: {stdout:?}"
    );
    assert!(
        !accepted_network_connection,
        "AppContainer without network capabilities connected to loopback"
    );
}

#[test]
fn appcontainer_child_probe() {
    let args = std::env::args().collect::<Vec<_>>();
    let Some(probe_index) = args.iter().position(|argument| argument == "--probe-child") else {
        return;
    };
    let outside_file = PathBuf::from(&args[probe_index + 1]);
    let workspace = PathBuf::from(&args[probe_index + 2]);
    let baseline = PathBuf::from(&args[probe_index + 3]);
    let network_probe_port = args[probe_index + 4]
        .parse::<u16>()
        .expect("probe port is numeric");

    std::fs::write(workspace.join("inside-workspace.txt"), "inside-ok")
        .expect("AppContainer writes its explicitly granted workspace");
    let read_denied = std::fs::read(&outside_file).is_err();
    let write_denied = std::fs::write(&outside_file, "unexpected").is_err();
    let baseline_denied = std::fs::read(baseline.join("appcontainer-probe.exe")).is_err();
    let network_denied = TcpStream::connect_timeout(
        &format!("127.0.0.1:{network_probe_port}")
            .parse()
            .expect("local probe address is valid"),
        Duration::from_secs(2),
    )
    .is_err();
    let result = format!(
        "read_denied={read_denied};write_denied={write_denied};baseline_denied={baseline_denied};network_denied={network_denied}"
    );
    std::fs::write(workspace.join("appcontainer-result.txt"), &result)
        .expect("AppContainer writes the probe result to its granted workspace");
    println!(
        "probe={result};ambient_secret_filtered={};explicit_env={}",
        std::env::var_os("OPENAI_API_KEY").is_none(),
        std::env::var_os("HAVEN_PROBE_EXPLICIT").as_deref()
            == Some(std::ffi::OsStr::new("visible")),
    );
}
