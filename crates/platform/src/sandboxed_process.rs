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

const PIPE_BUFFER_BYTES: usize = 16 * 1024;
const PIPE_QUEUE_CHUNKS: usize = 4;
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// One Windows process request with the filesystem and network rights the
/// caller's live authorization permit granted.
#[derive(Debug, Clone)]
pub struct SandboxedProcessRequest {
    pub program: OsString,
    pub arguments: Vec<OsString>,
    pub working_directory: PathBuf,
    pub readwrite_paths: Vec<PathBuf>,
    pub readonly_paths: Vec<PathBuf>,
    pub environment: Vec<(OsString, OsString)>,
    pub allow_network: bool,
    /// PowerShell needs desktop handles to initialize inside BaseContainer.
    /// The process still keeps clipboard, input injection, system settings,
    /// and desktop control restrictions.
    pub allow_desktop_handle_access: bool,
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
    /// When BaseContainer cannot enforce the requested policy, a compatibility
    /// retry narrows network access and accepts only an isolation tier MXC's
    /// probe confirms. A host process is never used as a fallback.
    pub async fn spawn(request: SandboxedProcessRequest) -> anyhow::Result<Self> {
        let process = tokio::task::spawn_blocking(move || {
            let requested = build_container_request(
                request.clone(),
                NetworkProfile::Requested,
            )?;
            let probe = probe_container_request(&requested)?;
            if probe_is_base_container(&probe) {
                let process = spawn_container_request(requested)?;
                return Ok((process, None));
            }

            let compatibility =
                build_container_request(request.clone(), NetworkProfile::OutboundOnly)?;
            let compatibility_probe = probe_container_request(&compatibility)?;
            let probe_error = compatibility_probe
                .error
                .as_deref()
                .map(haven_common::error::sanitize_error_text)
                .unwrap_or_else(|| "no compatible isolation tier was reported".to_owned());
            anyhow::ensure!(
                probe_is_compatible_fallback(&compatibility_probe),
                "MXC BaseContainer and AppContainer compatibility isolation are unavailable for this Windows process policy: {}",
                probe_error
            );

            let notice = compatibility_notice(&compatibility_probe, request.allow_network);
            let process = spawn_container_request(compatibility)?;
            let warning = compatibility_probe
                .warnings
                .iter()
                .map(|warning| haven_common::error::sanitize_error_text(warning))
                .collect::<Vec<_>>()
                .join("; ");
            tracing::warn!(
                tier = compatibility_probe.tier.unwrap_or("unknown"),
                network_egress_only = request.allow_network,
                warning = %warning,
                "MXC started the shell with compatibility isolation"
            );
            Ok((process, Some(notice)))
        })
        .await
        .map_err(|_| anyhow::anyhow!("MXC process launch task failed"))??;
        let (process, degradation_notice) = process;

        for warning in process.warnings() {
            tracing::warn!(warning = %haven_common::error::sanitize_error_text(&warning), "MXC sandbox reported a process warning");
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

    pub async fn wait(&self) -> io::Result<i32> {
        loop {
            if let Some(status) = self.try_wait().await? {
                return Ok(status);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
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
    anyhow::ensure!(
        !readwrite_paths.is_empty(),
        "MXC requires at least one authorized writable root"
    );
    let mut readonly_paths = normalize_directories(&request.readonly_paths, "read-only root")?;
    readonly_paths.retain(|readonly| {
        !readwrite_paths
            .iter()
            .any(|writable| path_contains(writable, readonly) || path_contains(readonly, writable))
    });
    if !readwrite_paths
        .iter()
        .any(|root| path_contains(root, &working_directory))
        && !readonly_paths
            .iter()
            .any(|root| path_contains(root, &working_directory))
    {
        readonly_paths.push(working_directory.clone());
    }

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
    let ingress_action = match network_profile {
        NetworkProfile::Requested => network_action,
        NetworkProfile::OutboundOnly => NetworkAction::Deny,
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
            ..Default::default()
        }),
        network: Some(NetworkPolicy {
            egress: Some(NetworkEgressPolicy {
                default: Some(network_action),
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
}

fn probe_container_request(
    request: &mxc_sdk::v1::ContainerRequest,
) -> anyhow::Result<mxc_sdk::v1::ProbeOutput> {
    mxc_sdk::v1::probe(Some(request))
        .map_err(|_| anyhow::anyhow!("MXC could not validate the Windows process policy"))
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
    mxc_sdk::v1::spawn(request, Default::default())
        .map_err(|_| anyhow::anyhow!("MXC could not start the Windows sandboxed process"))
}

fn compatibility_notice(probe: &mxc_sdk::v1::ProbeOutput, network_egress_only: bool) -> String {
    let tier = probe.tier.unwrap_or("unknown");
    let dacl_note = if probe.needs_dacl_augmentation == Some(true) {
        "兼容层会临时调整授权目录的 ACL，并在子进程结束时恢复。"
    } else {
        ""
    };
    let network_note = if network_egress_only {
        "网络权限已收窄为仅允许公网出站；私有网络入站和本机回环已禁用。"
    } else {
        "网络访问仍被完全禁用。"
    };
    format!("MXC 兼容隔离已启用（{tier}）。{dacl_note}{network_note}")
}

fn normalize_directories(paths: &[PathBuf], label: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut normalized: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        let path = normalize_directory(path, label)?;
        if !normalized.iter().any(|existing| same_path(existing, &path)) {
            normalized.push(path);
        }
    }
    Ok(normalized)
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

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn path_contains(root: &Path, candidate: &Path) -> bool {
    let root = root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase();
    let candidate = candidate
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase();
    candidate == root
        || candidate.starts_with(&format!("{root}\\"))
        || candidate.starts_with(&format!("{root}/"))
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
        NetworkProfile, SandboxedProcessRequest, build_container_request, quote_windows_argument,
    };
    use std::ffi::OsString;

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
