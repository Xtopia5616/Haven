//! Windows process isolation backed by MXC ProcessContainer.
//!
//! This is the OS boundary used by adapters that must run under
//! `WorkspaceWrite`. Requests carry explicit filesystem and network policy;
//! unsupported hosts and policies fail closed.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

use crate::filesystem::FilesystemPathCapability;

mod diagnostics;

const PIPE_BUFFER_BYTES: usize = 16 * 1024;
const PIPE_QUEUE_CHUNKS: usize = 4;
const READ_CHUNK_BYTES: usize = 8 * 1024;
const MAX_CAPTURED_DENIALS: usize = 64;
const MAX_CAPTURED_DENIALS_JSON_BYTES: u64 = 4 * 1024 * 1024;

/// One Windows process request with the filesystem and network rights the
/// caller's live authorization permit granted.
#[derive(Debug, Clone)]
pub struct SandboxedProcessRequest {
    pub program: OsString,
    pub arguments: Vec<OsString>,
    pub working_directory: PathBuf,
    pub readwrite_paths: Vec<PathBuf>,
    pub readonly_paths: Vec<PathBuf>,
    /// Explicit read-only protections. These paths override overlapping write
    /// roots and require a backend with native path policy enforcement.
    pub protected_readonly_paths: Vec<PathBuf>,
    /// Paths whose reads and writes must be denied.
    pub denied_paths: Vec<PathBuf>,
    pub environment: Vec<(OsString, OsString)>,
    pub allow_network: bool,
    /// PowerShell needs desktop handles to initialize inside BaseContainer.
    /// The process still keeps clipboard, input injection, system settings,
    /// and desktop control restrictions.
    pub allow_desktop_handle_access: bool,
}

/// Terminal status and structured policy denials from one isolated process.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxedProcessExit {
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_denials: Option<CapturedDenials>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_denials_error: Option<String>,
}

/// Denied resources observed by MXC while the process remained blocked.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CapturedDenials {
    pub total_denials: usize,
    pub denied_resources_truncated: bool,
    pub capture_complete: bool,
    pub denials: Vec<CapturedDeniedResource>,
}

/// One distinct denied resource and access type recorded by MXC.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CapturedDeniedResource {
    pub resource: String,
    pub resource_type: String,
    pub access_type: String,
    pub pid: u32,
    /// Decimal string preserves the 64-bit FILETIME across JSON/JavaScript.
    pub filetime: String,
}

/// A live MXC process with async pipes and cancellation-safe lifecycle calls.
pub struct SandboxedProcess {
    process: Arc<Mutex<mxc_sdk::v1::MxcProcess>>,
    stdin: Option<tokio::io::DuplexStream>,
    stdout: Option<SandboxedPipeReader>,
    stderr: Option<SandboxedPipeReader>,
    degradation_notice: Option<String>,
}

struct SandboxedPipeReader {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    current: Vec<u8>,
    offset: usize,
}

impl SandboxedProcess {
    /// Start the request in MXC's strongest compatible process-container tier.
    /// If the requested profile cannot be probed or started, compatibility
    /// retries progressively narrow network access. Every started process is
    /// still confined by an MXC tier; a host process is never used as fallback.
    pub async fn spawn(request: SandboxedProcessRequest) -> anyhow::Result<Self> {
        let process = tokio::task::spawn_blocking(move || {
            launch_with_fallback(request, probe_container_request, spawn_container_request)
        })
        .await
        .map_err(|_| anyhow::anyhow!("MXC process launch task failed"))??;
        let (process, mut degradation_notice) = process;

        let process_warnings = process.warnings().to_vec();
        for warning in &process_warnings {
            tracing::warn!(warning = %haven_common::error::sanitize_error_text(warning), "MXC sandbox reported a process warning");
        }
        if let Some(notice) = &mut degradation_notice {
            append_warning_notes(notice, &process_warnings);
        }

        let mut process = process;
        let stdin = process.take_stdin().map(make_async_writer);
        let stdout = process.take_stdout().map(make_async_reader);
        let stderr = process.take_stderr().map(make_async_reader);
        Ok(Self {
            process: Arc::new(Mutex::new(process)),
            stdin,
            stdout,
            stderr,
            degradation_notice,
        })
    }

    /// User-visible explanation when MXC had to use a lower isolation tier.
    pub fn degradation_notice(&self) -> Option<&str> {
        self.degradation_notice.as_deref()
    }

    pub fn take_stdin(&mut self) -> Option<impl AsyncWrite + Unpin + Send + 'static> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<impl AsyncRead + Unpin + Send + 'static> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<impl AsyncRead + Unpin + Send + 'static> {
        self.stderr.take()
    }

    pub async fn try_wait(&self) -> io::Result<Option<i32>> {
        let process = Arc::clone(&self.process);
        tokio::task::spawn_blocking(move || lock_process(&process).try_wait())
            .await
            .map_err(|_| io::Error::other("MXC process status task failed"))?
    }

    pub async fn wait(&self) -> io::Result<SandboxedProcessExit> {
        loop {
            match self.try_wait().await {
                Ok(Some(_)) => {
                    let process = Arc::clone(&self.process);
                    return tokio::task::spawn_blocking(move || {
                        let mut process = lock_process(&process);
                        // MXC finalizes structured output metadata as part of
                        // wait(), after the sandbox process exits and
                        // enforcement teardown completes. Keep polling with
                        // try_wait() first so cancellation can still acquire
                        // the process lock and terminate a running command.
                        match process.wait()? {
                            mxc_sdk::v1::WaitResult::Exited(exit_code) => {
                                let metadata = process.output_metadata().cloned();
                                Ok(capture_exit_metadata(exit_code, metadata))
                            }
                            mxc_sdk::v1::WaitResult::TimedOut => Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "MXC sandbox process timed out",
                            )),
                        }
                    })
                    .await
                    .map_err(|_| io::Error::other("MXC process wait task failed"))?;
                }
                Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    // A timeout is a terminal MXC outcome too; call wait()
                    // once so it can kill/reap the process tree and finalize
                    // capture cleanup before returning the timeout upstream.
                    let process = Arc::clone(&self.process);
                    let terminal = tokio::task::spawn_blocking(move || {
                        let mut process = lock_process(&process);
                        let terminal = process.wait()?;
                        let metadata = process.output_metadata().cloned();
                        Ok::<_, io::Error>((terminal, metadata))
                    })
                    .await
                    .map_err(|_| io::Error::other("MXC timeout cleanup task failed"))??;
                    return match terminal.0 {
                        mxc_sdk::v1::WaitResult::TimedOut => Err(error),
                        mxc_sdk::v1::WaitResult::Exited(exit_code) => {
                            Ok(capture_exit_metadata(exit_code, terminal.1))
                        }
                    };
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn kill(&self) -> io::Result<()> {
        let process = Arc::clone(&self.process);
        tokio::task::spawn_blocking(move || lock_process(&process).kill())
            .await
            .map_err(|_| io::Error::other("MXC process termination task failed"))?
    }
}

impl Drop for SandboxedProcess {
    fn drop(&mut self) {
        let process = Arc::clone(&self.process);
        let _ = std::thread::Builder::new()
            .name("haven-mxc-cleanup".into())
            .spawn(move || {
                let _ = lock_process(&process).kill();
            });
    }
}

impl AsyncRead for SandboxedPipeReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if self.offset < self.current.len() {
                let count = output
                    .remaining()
                    .min(self.current.len().saturating_sub(self.offset));
                output.put_slice(&self.current[self.offset..self.offset + count]);
                self.offset += count;
                if self.offset == self.current.len() {
                    self.current.clear();
                    self.offset = 0;
                }
                return Poll::Ready(Ok(()));
            }
            match Pin::new(&mut self.receiver).poll_recv(context) {
                Poll::Ready(Some(Ok(chunk))) => self.current = chunk,
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn make_async_reader(mut reader: Box<dyn Read + Send>) -> SandboxedPipeReader {
    let (sender, receiver) = mpsc::channel(PIPE_QUEUE_CHUNKS);
    let _reader_task = tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0; READ_CHUNK_BYTES];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if sender.blocking_send(Ok(buffer[..count].to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.blocking_send(Err(error));
                    break;
                }
            }
        }
    });
    SandboxedPipeReader {
        receiver,
        current: Vec::new(),
        offset: 0,
    }
}

fn make_async_writer(mut writer: Box<dyn Write + Send>) -> tokio::io::DuplexStream {
    let (async_writer, mut async_reader) = tokio::io::duplex(PIPE_BUFFER_BYTES);
    tokio::spawn(async move {
        let mut buffer = vec![0; READ_CHUNK_BYTES];
        loop {
            let count = match async_reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            let chunk = buffer[..count].to_vec();
            let (write_result, next_writer) = match tokio::task::spawn_blocking(move || {
                let result = writer.write_all(&chunk);
                (result, writer)
            })
            .await
            {
                Ok(result) => result,
                Err(_) => break,
            };
            if write_result.is_err() {
                break;
            }
            writer = next_writer;
        }
    });
    async_writer
}

fn build_container_request(
    request: SandboxedProcessRequest,
    network_profile: NetworkProfile,
) -> anyhow::Result<mxc_sdk::v1::ContainerRequest> {
    use mxc_sdk::v1::configs::{
        ProcessContainerConfig, ProcessContainerUi, ProcessContainerUiIsolation,
    };
    use mxc_sdk::v1::{
        ClipboardPolicy, ContainerRequest, Containment, FilesystemPolicy, NetworkAction,
        NetworkEgressPolicy, NetworkIngressPolicy, NetworkPolicy, UiPolicy,
    };

    let working_directory = normalize_directory(&request.working_directory, "working directory")?;
    let readwrite_paths = normalize_directories(&request.readwrite_paths, "writable root")?;
    let readonly_paths = normalize_policy_paths(&request.readonly_paths, "read-only root")?;
    let protected_readonly_paths = normalize_policy_paths(
        &request.protected_readonly_paths,
        "protected read-only path",
    )?;
    let denied_paths = normalize_policy_paths(&request.denied_paths, "denied path")?;

    // The authorization owner compiles path precedence before constructing
    // this request. The MXC adapter validates the final policy and serializes
    // it without widening or narrowing the owner's grants.
    anyhow::ensure!(
        !readwrite_paths.iter().any(|writable| {
            denied_paths
                .iter()
                .any(|denied| denied.contains_path(writable).unwrap_or(true))
                || protected_readonly_paths
                    .iter()
                    .any(|readonly| readonly.contains_path(writable).unwrap_or(true))
        }),
        "MXC request contains a writable root covered by a protected or denied path"
    );
    let mut readonly_paths = deduplicate_policy_paths(readonly_paths);
    readonly_paths.extend(protected_readonly_paths.iter().cloned());
    readonly_paths = deduplicate_policy_paths(readonly_paths);
    anyhow::ensure!(
        !readonly_paths.iter().any(|readonly| {
            denied_paths
                .iter()
                .any(|denied| denied.contains_path(readonly.path()).unwrap_or(true))
        }),
        "MXC request contains a read-only root covered by a denied path"
    );
    anyhow::ensure!(
        readwrite_paths
            .iter()
            .map(PathBuf::as_path)
            .chain(readonly_paths.iter().map(|path| path.path()))
            .any(|root| path_contains(root, &working_directory)),
        "MXC working directory is outside the authorized filesystem roots"
    );
    anyhow::ensure!(
        !denied_paths
            .iter()
            .any(|denied| denied.contains_path(&working_directory).unwrap_or(true)),
        "MXC working directory is explicitly denied"
    );
    let readonly_paths = readonly_paths
        .iter()
        .map(|path| path.path().to_path_buf())
        .collect::<Vec<_>>();

    let environment = request
        .environment
        .iter()
        .map(|(key, value)| Ok((os_to_string(key)?, os_to_string(value)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let command_line = std::iter::once(&request.program)
        .chain(request.arguments.iter())
        .map(|argument| os_to_string(argument).map(|value| quote_windows_argument(&value)))
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(" ");
    let network_action = if request.allow_network {
        NetworkAction::Allow
    } else {
        NetworkAction::Deny
    };
    let egress_action = match network_profile {
        NetworkProfile::Offline => NetworkAction::Deny,
        NetworkProfile::Requested | NetworkProfile::OutboundOnly => network_action,
    };
    let ingress_action = match network_profile {
        NetworkProfile::Requested => network_action,
        NetworkProfile::OutboundOnly | NetworkProfile::Offline => NetworkAction::Deny,
    };
    let ui = request.allow_desktop_handle_access.then_some(UiPolicy {
        disable: false,
        clipboard: ClipboardPolicy::None,
        allow_input_injection: false,
    });
    let mut process_container_ui = ProcessContainerUi::default();
    process_container_ui.isolation = if request.allow_desktop_handle_access {
        ProcessContainerUiIsolation::Desktop
    } else {
        ProcessContainerUiIsolation::Container
    };
    let mut process_container_config = ProcessContainerConfig::default();
    process_container_config.ui = Some(process_container_ui);
    let containment = Containment::ProcessContainer(process_container_config);

    Ok(ContainerRequest {
        filesystem: Some(FilesystemPolicy {
            readwrite_paths: readwrite_paths
                .iter()
                .map(|path| path_to_string(path))
                .collect(),
            readonly_paths: readonly_paths
                .iter()
                .map(|path| path_to_string(path))
                .collect(),
            denied_paths: denied_paths
                .iter()
                .map(|path| path.path().to_string_lossy().into_owned())
                .collect(),
            ..Default::default()
        }),
        network: Some(NetworkPolicy {
            egress: Some(NetworkEgressPolicy {
                default: Some(egress_action),
                ..Default::default()
            }),
            ingress: Some(NetworkIngressPolicy {
                default: Some(ingress_action),
                host_loopback: Some(ingress_action),
            }),
            runtime_config: None,
        }),
        ui,
        containment,
        working_directory: Some(path_to_string(&working_directory)),
        environment: Some(environment),
        // `shell_runtime` already supplies Haven's filtered environment. Do
        // not layer the user's ambient environment back into the sandbox.
        inherit_default_environment: Some(false),
        ..ContainerRequest::new(command_line)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkProfile {
    Requested,
    /// Keep only public outbound networking; deny private-network ingress and
    /// host loopback in the compatibility request.
    OutboundOnly,
    /// Disable all network access after the public-outbound compatibility
    /// profile could not be probed or started.
    Offline,
}

fn launch_with_fallback<T>(
    request: SandboxedProcessRequest,
    mut probe: impl FnMut(&mxc_sdk::v1::ContainerRequest) -> anyhow::Result<mxc_sdk::v1::ProbeOutput>,
    mut spawn: impl FnMut(mxc_sdk::v1::ContainerRequest) -> anyhow::Result<T>,
) -> anyhow::Result<(T, Option<String>)> {
    let requested = build_container_request(request.clone(), NetworkProfile::Requested)?;
    let mut failures = Vec::new();
    let (requires_native_filesystem_precedence, requires_native_denied_paths) =
        filesystem_policy_requirements(&request)?;
    let filesystem_requirement = if requires_native_denied_paths {
        ProbeRequirement::BaseContainerWithDeniedPaths
    } else if requires_native_filesystem_precedence {
        ProbeRequirement::BaseContainer
    } else {
        ProbeRequirement::CompatibleTier
    };
    let requested_requirement =
        if request.allow_network && filesystem_requirement == ProbeRequirement::CompatibleTier {
            ProbeRequirement::BaseContainer
        } else {
            filesystem_requirement
        };
    match launch_attempt(requested, requested_requirement, &mut probe, &mut spawn) {
        Ok((process, probe_output, capture_notice)) => {
            if probe_is_base_container(&probe_output) {
                return Ok((process, capture_notice));
            }
            let notice = compatibility_notice(
                &probe_output,
                NetworkProfile::Offline,
                request.allow_network,
                &[],
                &probe_output.warnings,
            );
            tracing::warn!(
                tier = probe_output.tier.unwrap_or("unknown"),
                network = "denied",
                warnings = %format_warnings(&probe_output.warnings),
                "MXC started the shell with compatibility isolation"
            );
            return Ok((process, Some(combine_notices(notice, capture_notice))));
        }
        Err(failure) => failures.push(AttemptFailure {
            profile: NetworkProfile::Requested,
            reason: failure.reason,
            warnings: failure.warnings,
        }),
    }

    for profile in compatibility_profiles(request.allow_network) {
        let candidate = match build_container_request(request.clone(), profile) {
            Ok(candidate) => candidate,
            Err(error) => {
                failures.push(AttemptFailure {
                    profile,
                    reason: format!(
                        "请求构造失败 [mxc.request_construction_failed]：{}",
                        haven_common::error::sanitize_error_text(&error.to_string())
                    ),
                    warnings: Vec::new(),
                });
                continue;
            }
        };
        match launch_attempt(candidate, filesystem_requirement, &mut probe, &mut spawn) {
            Ok((process, probe_output, capture_notice)) => {
                let notice = compatibility_notice(
                    &probe_output,
                    profile,
                    request.allow_network,
                    &failures,
                    &probe_output.warnings,
                );
                tracing::warn!(
                    tier = probe_output.tier.unwrap_or("unknown"),
                    network = network_profile_name(profile),
                    warnings = %format_warnings(&probe_output.warnings),
                    "MXC started the shell with compatibility isolation"
                );
                return Ok((process, Some(combine_notices(notice, capture_notice))));
            }
            Err(failure) => failures.push(AttemptFailure {
                profile,
                reason: failure.reason,
                warnings: failure.warnings,
            }),
        }
    }

    let warning_note = format_warning_note(
        &failures
            .iter()
            .flat_map(|failure| failure.warnings.iter().cloned())
            .collect::<Vec<_>>(),
    );
    let details = failures
        .iter()
        .map(|failure| {
            format!(
                "{}：{}",
                network_profile_name(failure.profile),
                failure.reason
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    anyhow::bail!(
        "MXC 无法在 WorkspaceWrite 隔离中启动 Shell；已尝试的策略：{}{warning_note}",
        details,
    )
}

fn combine_notices(isolation_notice: String, capture_notice: Option<String>) -> String {
    match capture_notice {
        Some(capture) => format!("{isolation_notice}\n{capture}"),
        None => isolation_notice,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeRequirement {
    BaseContainer,
    BaseContainerWithDeniedPaths,
    CompatibleTier,
}

#[derive(Debug)]
struct AttemptFailure {
    profile: NetworkProfile,
    reason: String,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct LaunchFailure {
    reason: String,
    warnings: Vec<String>,
}

fn launch_attempt<T>(
    mut request: mxc_sdk::v1::ContainerRequest,
    requirement: ProbeRequirement,
    probe: &mut impl FnMut(&mxc_sdk::v1::ContainerRequest) -> anyhow::Result<mxc_sdk::v1::ProbeOutput>,
    spawn: &mut impl FnMut(mxc_sdk::v1::ContainerRequest) -> anyhow::Result<T>,
) -> Result<(T, mxc_sdk::v1::ProbeOutput, Option<String>), LaunchFailure> {
    let mut probe_output = probe(&request).map_err(|error| LaunchFailure {
        reason: diagnostics::format_mxc_failure("探测", &error),
        warnings: Vec::new(),
    })?;
    diagnostics::log_probe_warnings(&probe_output.warnings);
    let compatible = match requirement {
        ProbeRequirement::BaseContainer => probe_is_base_container(&probe_output),
        ProbeRequirement::BaseContainerWithDeniedPaths => {
            probe_is_base_container(&probe_output)
                && probe_output.probes.base_container_supports_deny_paths
        }
        ProbeRequirement::CompatibleTier => probe_is_compatible_fallback(&probe_output),
    };
    if !compatible {
        return Err(LaunchFailure {
            reason: probe_rejection_reason(&probe_output, requirement),
            warnings: probe_output.warnings.clone(),
        });
    }
    let mut capture_notice = None;
    let base_request = request.clone();
    let capture_available = probe_output.probes.native_capture_available
        || probe_output.probes.guarded_capture_available;
    if capture_available {
        enable_denial_capture(&mut request);
        match probe(&request) {
            Ok(capture_probe)
                if probe_satisfies(&capture_probe, requirement)
                    && (capture_probe.probes.native_capture_available
                        || capture_probe.probes.guarded_capture_available) =>
            {
                probe_output = capture_probe;
            }
            Ok(capture_probe) => {
                capture_notice = Some(format!(
                    "MXC 当前隔离层未确认可用的结构化拒绝记录；进程仍按原策略隔离运行。({})",
                    if probe_satisfies(&capture_probe, requirement) {
                        "captureDenials 能力探测未通过".to_owned()
                    } else {
                        probe_rejection_reason(&capture_probe, requirement)
                    }
                ));
                request = base_request.clone();
            }
            Err(error) => {
                capture_notice = Some(format!(
                    "MXC 结构化拒绝记录不可用；进程仍按原策略隔离运行。({})",
                    diagnostics::format_mxc_failure("拒绝记录探测", &error)
                ));
                request = base_request.clone();
            }
        }
    } else {
        capture_notice = Some(
            "MXC 主机未提供结构化拒绝记录能力；无法列出 Shell 访问被拦截的具体资源。".to_owned(),
        );
    }
    let process = match spawn(request) {
        Ok(process) => process,
        Err(error) if capture_notice.is_none() => {
            let capture_error = diagnostics::format_mxc_failure("拒绝记录启动", &error);
            match spawn(base_request) {
                Ok(process) => {
                    capture_notice = Some(format!(
                        "MXC 无法启用结构化拒绝记录；进程仍按原策略隔离运行。({capture_error})"
                    ));
                    process
                }
                Err(error) => {
                    return Err(LaunchFailure {
                        reason: diagnostics::format_mxc_failure("启动", &error),
                        warnings: probe_output.warnings.clone(),
                    });
                }
            }
        }
        Err(error) => {
            return Err(LaunchFailure {
                reason: diagnostics::format_mxc_failure("启动", &error),
                warnings: probe_output.warnings.clone(),
            });
        }
    };
    Ok((process, probe_output, capture_notice))
}

fn probe_satisfies(probe: &mxc_sdk::v1::ProbeOutput, requirement: ProbeRequirement) -> bool {
    match requirement {
        ProbeRequirement::BaseContainer => probe_is_base_container(probe),
        ProbeRequirement::BaseContainerWithDeniedPaths => {
            probe_is_base_container(probe) && probe.probes.base_container_supports_deny_paths
        }
        ProbeRequirement::CompatibleTier => probe_is_compatible_fallback(probe),
    }
}

fn enable_denial_capture(request: &mut mxc_sdk::v1::ContainerRequest) {
    use mxc_sdk::v1::{
        Containment,
        configs::{CaptureDenials, CaptureDenialsMode},
    };

    if let Containment::ProcessContainer(config) = &mut request.containment {
        let mut capture = CaptureDenials::default();
        capture.mode = CaptureDenialsMode::Block;
        capture.output_path = None;
        capture.retain_etl = false;
        config.capture_denials = Some(capture);
    }
}

fn capture_exit_metadata(
    exit_code: i32,
    metadata: Option<mxc_sdk::v1::ExecutionMetadata>,
) -> SandboxedProcessExit {
    let Some(metadata) = metadata else {
        return SandboxedProcessExit {
            exit_code,
            capture_denials: None,
            capture_denials_error: Some("MXC 未返回结构化拒绝记录元数据".into()),
        };
    };
    let Some(output) = metadata.capture_denials else {
        return SandboxedProcessExit {
            exit_code,
            capture_denials: None,
            capture_denials_error: Some(
                metadata
                    .capture_denials_error
                    .map(|error| haven_common::error::sanitize_error_text(&error.message))
                    .unwrap_or_else(|| "MXC 未返回拒绝记录文件元数据".into()),
            ),
        };
    };

    let file_result = read_denials_document(&output.output_path);
    let _ = std::fs::remove_file(&output.output_path);
    match file_result {
        Ok(document) => {
            let summary = document.summary;
            let truncated_by_haven = document.denials.len() > MAX_CAPTURED_DENIALS;
            let summary_consistent = summary.exit_code == exit_code
                && summary.total_denials == output.total_denials
                && summary.total_denials == document.denials.len()
                && summary.denied_resources_truncated == output.denied_resources_truncated;
            let metadata_capture_error = metadata.capture_denials_error;
            let capture_complete = summary_consistent
                && metadata_capture_error.is_none()
                && !truncated_by_haven
                && !output.denied_resources_truncated;
            let capture_denials_error = metadata_capture_error
                .map(|error| haven_common::error::sanitize_error_text(&error.message))
                .or_else(|| {
                    (!summary_consistent).then(|| {
                        "MXC 拒绝记录摘要与进程输出元数据不一致；不会据此自动扩展权限".into()
                    })
                });
            SandboxedProcessExit {
                exit_code,
                capture_denials: Some(CapturedDenials {
                    total_denials: output.total_denials,
                    denied_resources_truncated: output.denied_resources_truncated
                        || summary.denied_resources_truncated
                        || truncated_by_haven
                        || !summary_consistent,
                    capture_complete,
                    denials: document
                        .denials
                        .into_iter()
                        .take(MAX_CAPTURED_DENIALS)
                        .map(|denial| CapturedDeniedResource {
                            resource: denial.resource,
                            resource_type: format!("{:?}", denial.resource_type)
                                .to_ascii_lowercase(),
                            access_type: format!("{:?}", denial.access_type).to_ascii_lowercase(),
                            pid: denial.pid,
                            filetime: denial.filetime.to_string(),
                        })
                        .collect(),
                }),
                capture_denials_error,
            }
        }
        Err(error) => SandboxedProcessExit {
            exit_code,
            capture_denials: Some(CapturedDenials {
                total_denials: output.total_denials,
                denied_resources_truncated: output.denied_resources_truncated,
                capture_complete: false,
                denials: Vec::new(),
            }),
            capture_denials_error: Some(haven_common::error::sanitize_error_text(&error)),
        },
    }
}

fn read_denials_document(
    path: &str,
) -> Result<mxc_sdk::learning_mode_core::DenialsDocument, String> {
    use std::io::Read;

    let file =
        std::fs::File::open(path).map_err(|error| format!("无法读取 MXC 拒绝记录文件：{error}"))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("无法检查 MXC 拒绝记录文件：{error}"))?
        .len();
    if file_len > MAX_CAPTURED_DENIALS_JSON_BYTES {
        return Err(format!(
            "MXC 拒绝记录文件超过 {} 字节上限",
            MAX_CAPTURED_DENIALS_JSON_BYTES
        ));
    }
    let capacity =
        usize::try_from(file_len).map_err(|_| "MXC 拒绝记录文件大小超出当前平台范围".to_owned())?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_CAPTURED_DENIALS_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取 MXC 拒绝记录文件失败：{error}"))?;
    if bytes.len() as u64 > MAX_CAPTURED_DENIALS_JSON_BYTES {
        return Err(format!(
            "MXC 拒绝记录文件超过 {} 字节上限",
            MAX_CAPTURED_DENIALS_JSON_BYTES
        ));
    }
    serde_json::from_slice::<mxc_sdk::learning_mode_core::DenialsDocument>(&bytes)
        .map_err(|error| format!("MXC 拒绝记录格式无效：{error}"))
}

fn probe_rejection_reason(
    probe: &mxc_sdk::v1::ProbeOutput,
    requirement: ProbeRequirement,
) -> String {
    if let Some(error) = probe.error.as_deref() {
        return format!(
            "探测拒绝该策略 [mxc.probe_rejected]：{}",
            haven_common::error::sanitize_error_text(error)
        );
    }
    match requirement {
        ProbeRequirement::BaseContainer => format!(
            "探测未找到可完整执行的 BaseContainer [mxc.base_container_unavailable]（tier={}，DACL补强={:?}）",
            probe.tier.unwrap_or("unknown"),
            probe.needs_dacl_augmentation
        ),
        ProbeRequirement::BaseContainerWithDeniedPaths
            if probe_is_base_container(probe)
                && !probe.probes.base_container_supports_deny_paths =>
        {
            "BaseContainer 主机不支持 MXC 原生 deniedPaths，拒绝启动以保留显式拒绝策略 [mxc.denied_paths_unsupported]".to_owned()
        }
        ProbeRequirement::BaseContainerWithDeniedPaths => format!(
            "探测未找到支持 deniedPaths 的 BaseContainer [mxc.base_container_denied_paths_unavailable]（tier={}，DACL补强={:?}）",
            probe.tier.unwrap_or("unknown"),
            probe.needs_dacl_augmentation
        ),
        ProbeRequirement::CompatibleTier => format!(
            "探测未找到可接受的兼容隔离层 [mxc.compatible_tier_unavailable]（tier={}，DACL补强={:?}）",
            probe.tier.unwrap_or("unknown"),
            probe.needs_dacl_augmentation
        ),
    }
}

fn compatibility_profiles(allow_network: bool) -> Vec<NetworkProfile> {
    if allow_network {
        vec![NetworkProfile::OutboundOnly, NetworkProfile::Offline]
    } else {
        vec![NetworkProfile::Offline]
    }
}

fn network_profile_name(profile: NetworkProfile) -> &'static str {
    match profile {
        NetworkProfile::Requested => "原始网络策略",
        NetworkProfile::OutboundOnly => "仅公网出站兼容策略",
        NetworkProfile::Offline => "完全断网兼容策略",
    }
}

fn probe_container_request(
    request: &mxc_sdk::v1::ContainerRequest,
) -> anyhow::Result<mxc_sdk::v1::ProbeOutput> {
    mxc_sdk::v1::probe(Some(request)).map_err(anyhow::Error::new)
}

fn probe_is_base_container(probe: &mxc_sdk::v1::ProbeOutput) -> bool {
    probe.error.is_none()
        && probe.tier == Some("base-container")
        && probe.needs_dacl_augmentation == Some(false)
}

fn probe_is_compatible_fallback(probe: &mxc_sdk::v1::ProbeOutput) -> bool {
    compatible_fallback_tier(
        probe.tier,
        probe.error.is_some(),
        probe.needs_dacl_augmentation,
    )
}

fn compatible_fallback_tier(
    tier: Option<&str>,
    has_error: bool,
    needs_dacl_augmentation: Option<bool>,
) -> bool {
    if has_error {
        return false;
    }
    match tier {
        Some("base-container") => needs_dacl_augmentation == Some(false),
        Some("appcontainer-bfs") => needs_dacl_augmentation == Some(false),
        Some("appcontainer-dacl") => needs_dacl_augmentation == Some(true),
        _ => false,
    }
}

fn spawn_container_request(
    request: mxc_sdk::v1::ContainerRequest,
) -> anyhow::Result<mxc_sdk::v1::MxcProcess> {
    mxc_sdk::v1::spawn(request, Default::default()).map_err(anyhow::Error::new)
}

fn compatibility_notice(
    probe: &mxc_sdk::v1::ProbeOutput,
    profile: NetworkProfile,
    originally_allowed_network: bool,
    failures: &[AttemptFailure],
    warnings: &[String],
) -> String {
    let tier = probe.tier.unwrap_or("unknown");
    let dacl_note = if probe.needs_dacl_augmentation == Some(true) {
        "兼容层会临时调整授权目录的 ACL，并在子进程结束时恢复。"
    } else {
        ""
    };
    let network_note = match profile {
        NetworkProfile::Requested => "网络访问保持原授权策略。",
        NetworkProfile::OutboundOnly => {
            "网络权限已降级为仅允许公网出站；私有网络入站和本机回环已禁用。"
        }
        NetworkProfile::Offline if originally_allowed_network => {
            "公网出站兼容策略也无法启动；网络权限已进一步降级为完全禁用。"
        }
        NetworkProfile::Offline => "网络访问按原授权策略保持完全禁用。",
    };
    let failure_note = if failures.is_empty() {
        String::new()
    } else {
        let details = failures
            .iter()
            .rev()
            .take(2)
            .map(|failure| {
                format!(
                    "{}失败：{}",
                    network_profile_name(failure.profile),
                    failure.reason
                )
            })
            .collect::<Vec<_>>()
            .join("；");
        format!("前一档策略未能启动（{details}）。")
    };
    let all_warnings = failures
        .iter()
        .flat_map(|failure| failure.warnings.iter().cloned())
        .chain(warnings.iter().cloned())
        .collect::<Vec<_>>();
    let warning_note = format_warning_note(&all_warnings);
    format!("MXC 兼容隔离已启用（{tier}）。{failure_note}{dacl_note}{network_note}{warning_note}")
}

fn format_warnings(warnings: &[String]) -> String {
    diagnostics::format_warnings(warnings)
}

fn format_warning_note(warnings: &[String]) -> String {
    let warnings = format_warnings(warnings);
    if warnings.is_empty() {
        String::new()
    } else {
        format!("主机准备提示：{warnings}")
    }
}

fn append_warning_notes(notice: &mut String, warnings: &[String]) {
    let note = format_warning_note(warnings);
    if !note.is_empty() && !notice.contains(&note) {
        notice.push_str(&note);
    }
}

fn normalize_directories(paths: &[PathBuf], label: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut normalized: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        let path = normalize_directory(path, label)?;
        let identity = FilesystemPathCapability::open(&path)
            .map_err(|_| anyhow::anyhow!("MXC {label} identity is unavailable"))?;
        if !normalized.iter().any(|existing| {
            FilesystemPathCapability::open(existing)
                .is_ok_and(|existing| existing.same_object(&identity))
        }) {
            normalized.push(path);
        }
    }
    Ok(normalized)
}

fn normalize_policy_paths(
    paths: &[PathBuf],
    label: &str,
) -> anyhow::Result<Vec<FilesystemPathCapability>> {
    paths
        .iter()
        .map(|path| {
            FilesystemPathCapability::open(path)
                .map_err(|_| anyhow::anyhow!("MXC {label} is unavailable or unsafe"))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(deduplicate_policy_paths)
}

fn deduplicate_policy_paths(paths: Vec<FilesystemPathCapability>) -> Vec<FilesystemPathCapability> {
    let mut normalized = Vec::with_capacity(paths.len());
    for candidate in paths {
        if !normalized
            .iter()
            .any(|existing: &FilesystemPathCapability| existing.same_object(&candidate))
        {
            normalized.push(candidate);
        }
    }
    normalized
}

fn filesystem_policy_requirements(
    request: &SandboxedProcessRequest,
) -> anyhow::Result<(bool, bool)> {
    let writable = normalize_directories(&request.readwrite_paths, "writable root")?;
    let readonly = normalize_policy_paths(&request.readonly_paths, "read-only root")?;
    let protected = normalize_policy_paths(
        &request.protected_readonly_paths,
        "protected read-only path",
    )?;
    let writable = normalize_policy_paths(&writable, "writable root")?;

    // Any read/write overlap needs MXC's native, most-specific path handling:
    // a writable child under a broader read scope, and protected read-only
    // children under a writable root. Ancestor protections are removed from
    // the write grant by the authorization owner before this request is built.
    let mut requires_native_precedence = false;
    for write_root in &writable {
        for readonly in readonly.iter().chain(&protected) {
            if write_root.contains_path(readonly.path()).unwrap_or(true)
                || readonly.contains_path(write_root.path()).unwrap_or(true)
            {
                requires_native_precedence = true;
            }
        }
    }

    // MXC only exposes native deniedPaths on some BaseContainer hosts. A deny
    // that intersects an effective read grant or a descendant of a write
    // grant must not fall back to a tier that cannot enforce it. Ancestor
    // denies remove the whole grant in the authorization owner.
    let denied = normalize_policy_paths(&request.denied_paths, "denied path")?;
    let readonly_overlaps_deny = readonly.iter().chain(&protected).any(|root| {
        denied.iter().any(|deny| {
            deny.contains_path(root.path()).unwrap_or(true)
                || root.contains_path(deny.path()).unwrap_or(true)
        })
    });
    let writable_descendant_deny = writable.iter().any(|root| {
        denied.iter().any(|deny| {
            !deny.contains_path(root.path()).unwrap_or(true)
                && root.contains_path(deny.path()).unwrap_or(true)
        })
    });

    Ok((
        requires_native_precedence,
        readonly_overlaps_deny || writable_descendant_deny,
    ))
}

fn normalize_directory(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
    let canonical =
        std::fs::canonicalize(path).map_err(|_| anyhow::anyhow!("MXC {label} is unavailable"))?;
    anyhow::ensure!(canonical.is_dir(), "MXC {label} must be a directory");
    let raw_value = canonical.to_string_lossy().into_owned();
    let value = raw_value.strip_prefix("\\\\?\\").unwrap_or(&raw_value);
    let normalized = PathBuf::from(value);
    anyhow::ensure!(
        normalized.is_absolute() && !value.starts_with("\\\\"),
        "MXC {label} must be a local absolute directory"
    );
    Ok(normalized)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn path_contains(root: &Path, candidate: &Path) -> bool {
    let root = root.components().collect::<Vec<_>>();
    let candidate = candidate.components().collect::<Vec<_>>();
    candidate.len() >= root.len()
        && root.iter().zip(candidate.iter()).all(|(left, right)| {
            left.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
        })
}

fn os_to_string(value: &OsStr) -> anyhow::Result<String> {
    value.to_str().map(str::to_owned).ok_or_else(|| {
        anyhow::anyhow!("MXC does not support non-Unicode Windows process arguments")
    })
}

fn quote_windows_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.chars().any(char::is_whitespace) && !argument.contains('"')
    {
        return argument.to_owned();
    }

    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

fn lock_process(
    process: &Mutex<mxc_sdk::v1::MxcProcess>,
) -> std::sync::MutexGuard<'_, mxc_sdk::v1::MxcProcess> {
    process
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{
        NetworkProfile, ProbeRequirement, SandboxedProcessRequest, build_container_request,
        compatibility_profiles, filesystem_policy_requirements, launch_with_fallback,
        probe_satisfies, quote_windows_argument,
    };
    use std::ffi::OsString;

    fn shell_request(root: &std::path::Path, allow_network: bool) -> SandboxedProcessRequest {
        SandboxedProcessRequest {
            program: OsString::from("cmd"),
            arguments: Vec::new(),
            working_directory: root.to_path_buf(),
            readwrite_paths: vec![root.to_path_buf()],
            readonly_paths: Vec::new(),
            protected_readonly_paths: Vec::new(),
            denied_paths: Vec::new(),
            environment: Vec::new(),
            allow_network,
            allow_desktop_handle_access: false,
        }
    }

    fn probe_output(
        tier: &'static str,
        needs_dacl_augmentation: bool,
        warnings: Vec<String>,
    ) -> mxc_sdk::v1::ProbeOutput {
        use mxc_sdk::v1::{ProbeFacts, ProbeOutput, UiCapabilitySupport};

        ProbeOutput {
            tier: Some(tier),
            needs_dacl_augmentation: Some(needs_dacl_augmentation),
            warnings,
            probes: ProbeFacts {
                base_container_api_present: true,
                native_capture_available: false,
                guarded_capture_available: false,
                bfscfg_present: false,
                bfs_compiled_in: false,
                base_container_supports_deny_paths: true,
                base_container_supports_enumerate_paths: false,
                base_container_supports_ingress_host_loopback_allow: false,
                base_container_supports_identityless_loopback_proxy: false,
                isolation_session_available: false,
                hyperlight_available: false,
                ui_capabilities: UiCapabilitySupport {
                    can_block_clipboard_read: true,
                    can_block_clipboard_write: true,
                    can_block_input_injection: true,
                    can_block_input_method_changes: true,
                    can_block_external_ui_objects: true,
                    can_block_global_ui_namespace: true,
                    can_block_desktop_switching: true,
                    can_block_logoff_or_shutdown: true,
                    can_block_system_parameter_changes: true,
                    can_block_display_settings_changes: true,
                },
            },
            error: None,
        }
    }

    fn network_allows(request: &mxc_sdk::v1::ContainerRequest) -> (bool, bool, bool) {
        use mxc_sdk::v1::NetworkAction;

        let network = request.network.as_ref().expect("network policy is present");
        let egress = network.egress.as_ref().expect("egress policy is present");
        let ingress = network.ingress.as_ref().expect("ingress policy is present");
        (
            egress.default == Some(NetworkAction::Allow),
            ingress.default == Some(NetworkAction::Allow),
            ingress.host_loopback == Some(NetworkAction::Allow),
        )
    }

    #[test]
    fn windows_arguments_follow_createprocess_quoting_rules() {
        for (input, expected) in [
            ("", "\"\""),
            ("plain", "plain"),
            ("two words", "\"two words\""),
            ("quote\"inside", "\"quote\\\"inside\""),
            ("ends with slash\\", "\"ends with slash\\\\\""),
            ("a\\\"b", "\"a\\\\\\\"b\""),
        ] {
            assert_eq!(quote_windows_argument(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn container_request_does_not_inherit_ambient_environment() {
        let root = tempfile::tempdir().expect("create MXC environment fixture");
        let request = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                readwrite_paths: vec![root.path().to_path_buf()],
                readonly_paths: Vec::new(),
                protected_readonly_paths: Vec::new(),
                denied_paths: Vec::new(),
                environment: vec![(
                    OsString::from("PATH"),
                    OsString::from("C:\\Windows\\System32"),
                )],
                allow_network: false,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::Requested,
        )
        .expect("build MXC request");

        assert_eq!(request.inherit_default_environment, Some(false));
        assert_eq!(
            request.environment,
            Some(vec![(
                "PATH".to_owned(),
                "C:\\Windows\\System32".to_owned()
            )])
        );
        assert!(request.ui.is_none());
        let mxc_sdk::v1::Containment::ProcessContainer(config) = request.containment else {
            panic!("Windows shell requests use ProcessContainer containment");
        };
        assert_eq!(
            config
                .ui
                .expect("ProcessContainer UI defaults are explicit")
                .isolation,
            mxc_sdk::v1::configs::ProcessContainerUiIsolation::Container
        );
    }

    #[test]
    fn container_request_preserves_authorized_filesystem_roots_and_accepts_read_only_policy() {
        let root = tempfile::tempdir().expect("create MXC read-only fixture");
        let request = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                readwrite_paths: Vec::new(),
                readonly_paths: vec![root.path().to_path_buf()],
                protected_readonly_paths: Vec::new(),
                denied_paths: Vec::new(),
                environment: Vec::new(),
                allow_network: false,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::Requested,
        )
        .expect("MXC can start a process with a read-only filesystem policy");

        let filesystem = request.filesystem.expect("filesystem policy is explicit");
        assert!(filesystem.readwrite_paths.is_empty());
        assert_eq!(
            filesystem.readonly_paths,
            vec![root.path().to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn container_request_does_not_add_unrequested_working_directory_access() {
        let allowed = tempfile::tempdir().expect("create authorized MXC root");
        let outside = tempfile::tempdir().expect("create outside MXC root");
        let result = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: outside.path().to_path_buf(),
                readwrite_paths: vec![allowed.path().to_path_buf()],
                readonly_paths: Vec::new(),
                protected_readonly_paths: Vec::new(),
                denied_paths: Vec::new(),
                environment: Vec::new(),
                allow_network: false,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::Requested,
        );

        assert!(
            result
                .expect_err("working directory outside explicit roots must fail closed")
                .to_string()
                .contains("outside the authorized filesystem roots")
        );
    }

    #[test]
    fn container_request_rejects_a_working_directory_inside_a_denied_path() {
        let root = tempfile::tempdir().expect("create MXC deny fixture");
        let denied = root.path().join("private");
        std::fs::create_dir(&denied).expect("create denied directory");
        let error = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: denied.clone(),
                readwrite_paths: vec![root.path().to_path_buf()],
                readonly_paths: Vec::new(),
                protected_readonly_paths: Vec::new(),
                denied_paths: vec![denied],
                environment: Vec::new(),
                allow_network: false,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::Requested,
        )
        .expect_err("MXC must not start with a working directory under a hard deny");

        assert!(
            error
                .to_string()
                .contains("working directory is explicitly denied")
        );
    }

    #[test]
    fn container_request_keeps_overlapping_read_and_write_grants_from_owner() {
        let root = tempfile::tempdir().expect("create MXC filesystem fixture");
        let writable = root.path().join("src");
        std::fs::create_dir(&writable).expect("create nested writable root");
        let request = SandboxedProcessRequest {
            program: OsString::from("cmd"),
            arguments: Vec::new(),
            working_directory: writable.clone(),
            readwrite_paths: vec![writable.clone()],
            readonly_paths: vec![root.path().to_path_buf()],
            protected_readonly_paths: Vec::new(),
            denied_paths: Vec::new(),
            environment: Vec::new(),
            allow_network: false,
            allow_desktop_handle_access: false,
        };
        assert_eq!(
            filesystem_policy_requirements(&request).unwrap(),
            (true, false),
            "a writable child under a broader read grant needs native precedence"
        );
        let request = build_container_request(request, NetworkProfile::Requested)
            .expect("preserve the authorization owner's filesystem grants");

        let filesystem = request.filesystem.expect("filesystem policy is explicit");
        assert_eq!(
            filesystem.readwrite_paths,
            vec![writable.to_string_lossy().into_owned()]
        );
        assert_eq!(
            filesystem.readonly_paths,
            vec![root.path().to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn container_request_preserves_a_readonly_subdirectory_inside_a_write_root() {
        let root = tempfile::tempdir().expect("create MXC filesystem fixture");
        let protected = root.path().join(".git");
        std::fs::create_dir(&protected).expect("create protected subtree");
        let request = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                readwrite_paths: vec![root.path().to_path_buf()],
                readonly_paths: Vec::new(),
                protected_readonly_paths: vec![protected.clone()],
                denied_paths: Vec::new(),
                environment: Vec::new(),
                allow_network: false,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::Requested,
        )
        .expect("build overlapping MXC path policy");

        let filesystem = request.filesystem.expect("filesystem policy is explicit");
        assert_eq!(filesystem.readwrite_paths, [root.path().to_string_lossy()]);
        assert_eq!(filesystem.readonly_paths, [protected.to_string_lossy()]);
    }

    #[test]
    fn explicit_denied_subpath_is_retained_and_requires_native_enforcement() {
        let root = tempfile::tempdir().expect("create MXC filesystem fixture");
        let denied = root.path().join("private");
        std::fs::create_dir(&denied).expect("create denied subtree");
        let request = SandboxedProcessRequest {
            program: OsString::from("cmd"),
            arguments: Vec::new(),
            working_directory: root.path().to_path_buf(),
            readwrite_paths: vec![root.path().to_path_buf()],
            readonly_paths: Vec::new(),
            protected_readonly_paths: Vec::new(),
            denied_paths: vec![denied.clone()],
            environment: Vec::new(),
            allow_network: false,
            allow_desktop_handle_access: false,
        };
        assert_eq!(
            filesystem_policy_requirements(&request).unwrap(),
            (false, true)
        );

        let request = build_container_request(request, NetworkProfile::Requested)
            .expect("build MXC explicit deny policy");
        let filesystem = request.filesystem.expect("filesystem policy is explicit");
        assert_eq!(filesystem.readwrite_paths, [root.path().to_string_lossy()]);
        assert_eq!(filesystem.denied_paths, [denied.to_string_lossy()]);
    }

    #[test]
    fn container_request_rejects_a_readonly_grant_under_a_denied_ancestor() {
        let root = tempfile::tempdir().expect("create MXC filesystem fixture");
        let denied = root.path().join("private");
        let readonly = denied.join("nested");
        std::fs::create_dir_all(&readonly).expect("create nested protected path");
        let error = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                readwrite_paths: vec![root.path().to_path_buf()],
                readonly_paths: vec![readonly],
                protected_readonly_paths: Vec::new(),
                denied_paths: vec![denied.clone()],
                environment: Vec::new(),
                allow_network: false,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::Requested,
        )
        .expect_err("the authorization owner must remove read grants under denied ancestors");
        assert!(
            error
                .to_string()
                .contains("read-only root covered by a denied path")
        );
    }

    #[test]
    fn denied_path_policy_requires_host_capability_probe_to_succeed() {
        let mut probe = probe_output("base-container", false, Vec::new());
        assert!(probe_satisfies(
            &probe,
            ProbeRequirement::BaseContainerWithDeniedPaths
        ));

        probe.probes.base_container_supports_deny_paths = false;
        assert!(!probe_satisfies(
            &probe,
            ProbeRequirement::BaseContainerWithDeniedPaths
        ));
    }

    #[test]
    fn powershell_desktop_compatibility_keeps_other_ui_restrictions() {
        use mxc_sdk::v1::configs::{ProcessContainerSystemSettings, ProcessContainerUiIsolation};
        use mxc_sdk::v1::{ClipboardPolicy, Containment};

        let root = tempfile::tempdir().expect("create MXC UI fixture");
        let request = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("powershell.exe"),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                readwrite_paths: vec![root.path().to_path_buf()],
                readonly_paths: Vec::new(),
                protected_readonly_paths: Vec::new(),
                denied_paths: Vec::new(),
                environment: Vec::new(),
                allow_network: false,
                allow_desktop_handle_access: true,
            },
            NetworkProfile::Requested,
        )
        .expect("build PowerShell MXC request");

        let ui = request
            .ui
            .expect("PowerShell requires Win32 UI initialization");
        assert!(!ui.disable, "PowerShell requires Win32 system calls");
        assert_eq!(ui.clipboard, ClipboardPolicy::None);
        assert!(!ui.allow_input_injection);

        let Containment::ProcessContainer(config) = request.containment else {
            panic!("Windows shell requests use ProcessContainer containment");
        };
        let ui = config
            .ui
            .expect("ProcessContainer UI settings are explicit");
        assert_eq!(ui.isolation, ProcessContainerUiIsolation::Desktop);
        assert!(!ui.desktop_system_control);
        assert_eq!(ui.system_settings, ProcessContainerSystemSettings::None);
        assert!(!ui.ime);
    }

    #[test]
    fn compatibility_network_profile_keeps_only_requested_egress() {
        use mxc_sdk::v1::NetworkAction;

        let root = tempfile::tempdir().expect("create MXC network fixture");
        let request = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                readwrite_paths: vec![root.path().to_path_buf()],
                readonly_paths: Vec::new(),
                protected_readonly_paths: Vec::new(),
                denied_paths: Vec::new(),
                environment: Vec::new(),
                allow_network: true,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::OutboundOnly,
        )
        .expect("build compatibility MXC request");

        let network = request.network.expect("network policy is explicit");
        assert_eq!(
            network.egress.and_then(|policy| policy.default),
            Some(NetworkAction::Allow)
        );
        let ingress = network.ingress.expect("ingress policy is explicit");
        assert_eq!(ingress.default, Some(NetworkAction::Deny));
        assert_eq!(ingress.host_loopback, Some(NetworkAction::Deny));
        assert_eq!(network.runtime_config, None);
    }

    #[test]
    fn denied_network_stays_denied_in_compatibility_profile() {
        use mxc_sdk::v1::NetworkAction;

        let root = tempfile::tempdir().expect("create MXC network fixture");
        let request = build_container_request(
            SandboxedProcessRequest {
                program: OsString::from("cmd"),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                readwrite_paths: vec![root.path().to_path_buf()],
                readonly_paths: Vec::new(),
                protected_readonly_paths: Vec::new(),
                denied_paths: Vec::new(),
                environment: Vec::new(),
                allow_network: false,
                allow_desktop_handle_access: false,
            },
            NetworkProfile::OutboundOnly,
        )
        .expect("build compatibility MXC request");

        let network = request.network.expect("network policy is explicit");
        assert_eq!(
            network.egress.and_then(|policy| policy.default),
            Some(NetworkAction::Deny)
        );
        let ingress = network.ingress.expect("ingress policy is explicit");
        assert_eq!(ingress.default, Some(NetworkAction::Deny));
        assert_eq!(ingress.host_loopback, Some(NetworkAction::Deny));
    }

    #[test]
    fn offline_profile_denies_egress_ingress_and_host_loopback() {
        use mxc_sdk::v1::NetworkAction;

        let root = tempfile::tempdir().expect("create offline MXC fixture");
        let request =
            build_container_request(shell_request(root.path(), true), NetworkProfile::Offline)
                .expect("build offline MXC request");

        let network = request.network.expect("network policy is explicit");
        assert_eq!(
            network.egress.and_then(|policy| policy.default),
            Some(NetworkAction::Deny)
        );
        let ingress = network.ingress.expect("ingress policy is explicit");
        assert_eq!(ingress.default, Some(NetworkAction::Deny));
        assert_eq!(ingress.host_loopback, Some(NetworkAction::Deny));
    }

    #[test]
    fn probe_api_error_still_retries_with_public_outbound_compatibility() {
        let root = tempfile::tempdir().expect("create MXC retry fixture");
        let mut probe_calls = 0;
        let mut probed_network = Vec::new();
        let mut spawned_network = Vec::new();
        let (process, notice) = launch_with_fallback(
            shell_request(root.path(), true),
            |request| {
                probe_calls += 1;
                probed_network.push(network_allows(request));
                if probe_calls == 1 {
                    anyhow::bail!("initial probe API unavailable");
                }
                Ok(probe_output(
                    "appcontainer-dacl",
                    true,
                    vec!["host preparation is missing".into()],
                ))
            },
            |request| {
                spawned_network.push(network_allows(&request));
                Ok("started")
            },
        )
        .expect("compatibility retry should start the isolated process");

        assert_eq!(process, "started");
        assert_eq!(probed_network, [(true, true, true), (true, false, false)]);
        assert_eq!(spawned_network, [(true, false, false)]);
        let notice = notice.expect("compatibility startup is user-visible");
        assert!(notice.contains("仅允许公网出站"));
        assert!(notice.contains("initial probe API unavailable"));
        assert!(notice.contains("host preparation is missing"));
    }

    #[test]
    fn failed_public_outbound_start_retries_with_network_disabled() {
        let root = tempfile::tempdir().expect("create offline fallback fixture");
        let mut probe_calls = 0;
        let mut spawn_calls = 0;
        let mut probed_network = Vec::new();
        let mut spawned_network = Vec::new();
        let (process, notice) = launch_with_fallback(
            shell_request(root.path(), true),
            |request| {
                probe_calls += 1;
                probed_network.push(network_allows(request));
                Ok(match probe_calls {
                    1 => probe_output("base-container", false, Vec::new()),
                    2 => probe_output(
                        "appcontainer-dacl",
                        true,
                        vec!["Run `wxc-host-prep prepare-system-drive` (elevated).".into()],
                    ),
                    _ => probe_output(
                        "appcontainer-dacl",
                        true,
                        vec!["Run `wxc-host-prep prepare-null-device` (elevated).".into()],
                    ),
                })
            },
            |request| {
                spawn_calls += 1;
                spawned_network.push(network_allows(&request));
                match spawn_calls {
                    1 => anyhow::bail!("requested profile spawn failed"),
                    2 => anyhow::bail!("public egress setup failed"),
                    _ => Ok("started-offline"),
                }
            },
        )
        .expect("offline compatibility should keep the isolated shell available");

        assert_eq!(process, "started-offline");
        assert_eq!(
            probed_network,
            [
                (true, true, true),
                (true, false, false),
                (false, false, false)
            ]
        );
        assert_eq!(
            spawned_network,
            [
                (true, true, true),
                (true, false, false),
                (false, false, false)
            ]
        );
        let notice = notice.expect("offline downgrade is user-visible");
        assert!(notice.contains("完全禁用"));
        assert!(notice.contains("public egress setup failed"));
        assert!(notice.contains("mxc.host_prep.system_drive_acl_missing"));
        assert!(notice.contains("wxc-host-prep.exe prepare-system-drive"));
        assert!(notice.contains("mxc.host_prep.null_device_acl_missing"));
        assert!(notice.contains("wxc-host-prep.exe prepare-null-device"));
    }

    #[test]
    fn terminal_launch_failure_keeps_host_prep_actions_visible() {
        let root = tempfile::tempdir().expect("create terminal failure fixture");
        let mut probe_calls = 0;
        let error = launch_with_fallback(
            shell_request(root.path(), true),
            |_| {
                probe_calls += 1;
                Ok(match probe_calls {
                    1 => probe_output("base-container", false, Vec::new()),
                    2 => probe_output(
                        "appcontainer-dacl",
                        true,
                        vec!["wxc-host-prep prepare-system-drive is required".into()],
                    ),
                    _ => probe_output(
                        "appcontainer-dacl",
                        true,
                        vec!["wxc-host-prep prepare-null-device is required".into()],
                    ),
                })
            },
            |_| -> anyhow::Result<()> { anyhow::bail!("MXC denied process creation") },
        )
        .expect_err("all isolated launch tiers should fail in this fixture")
        .to_string();

        assert!(error.contains("启动失败 [mxc.unknown_api_error]"));
        assert!(error.contains("mxc.host_prep.system_drive_acl_missing"));
        assert!(error.contains("wxc-host-prep.exe prepare-system-drive"));
        assert!(error.contains("mxc.host_prep.null_device_acl_missing"));
        assert!(error.contains("wxc-host-prep.exe prepare-null-device"));
    }

    #[test]
    fn compatibility_profile_order_only_drops_network_permissions() {
        assert_eq!(
            compatibility_profiles(true),
            vec![NetworkProfile::OutboundOnly, NetworkProfile::Offline]
        );
        assert_eq!(compatibility_profiles(false), vec![NetworkProfile::Offline]);
    }

    #[test]
    fn compatibility_tier_accepts_mxc_appcontainer_and_clean_base_container() {
        use super::compatible_fallback_tier;

        assert!(compatible_fallback_tier(
            Some("appcontainer-dacl"),
            false,
            Some(true)
        ));
        assert!(compatible_fallback_tier(
            Some("appcontainer-bfs"),
            false,
            Some(false)
        ));
        assert!(compatible_fallback_tier(
            Some("base-container"),
            false,
            Some(false)
        ));
    }

    #[test]
    fn compatibility_tier_rejects_probe_errors_and_unenforced_base_requests() {
        use super::compatible_fallback_tier;

        assert!(!compatible_fallback_tier(
            Some("appcontainer-dacl"),
            true,
            Some(true)
        ));
        assert!(!compatible_fallback_tier(
            Some("base-container"),
            false,
            Some(true)
        ));
        assert!(!compatible_fallback_tier(
            Some("appcontainer-bfs"),
            false,
            Some(true)
        ));
        assert!(!compatible_fallback_tier(
            Some("appcontainer-dacl"),
            false,
            Some(false)
        ));
        assert!(!compatible_fallback_tier(
            Some("appcontainer-dacl"),
            false,
            None
        ));
        assert!(!compatible_fallback_tier(
            Some("unknown"),
            false,
            Some(false)
        ));
        assert!(!compatible_fallback_tier(None, false, None));
    }
}
