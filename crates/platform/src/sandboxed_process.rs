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
}

/// A live MXC process with async pipes and cancellation-safe lifecycle calls.
pub struct SandboxedProcess {
    process: Arc<Mutex<mxc_sdk::v1::MxcProcess>>,
    stdin: Option<tokio::io::DuplexStream>,
    stdout: Option<SandboxedPipeReader>,
    stderr: Option<SandboxedPipeReader>,
}

struct SandboxedPipeReader {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    current: Vec<u8>,
    offset: usize,
}

impl SandboxedProcess {
    /// Start the request only when MXC confirms that BaseContainer can enforce
    /// its full policy. No host-process fallback is attempted.
    pub async fn spawn(request: SandboxedProcessRequest) -> anyhow::Result<Self> {
        let container_request = build_container_request(request)?;
        let process = tokio::task::spawn_blocking(move || {
            let probe = mxc_sdk::v1::probe(Some(&container_request)).map_err(|_| {
                anyhow::anyhow!("MXC could not validate the Windows process policy")
            })?;
            anyhow::ensure!(
                probe.error.is_none()
                    && probe.tier == Some("base-container")
                    && probe.needs_dacl_augmentation == Some(false),
                "MXC BaseContainer isolation is unavailable for this Windows process policy"
            );
            mxc_sdk::v1::spawn(container_request, Default::default())
                .map_err(|_| anyhow::anyhow!("MXC could not start the Windows sandboxed process"))
        })
        .await
        .map_err(|_| anyhow::anyhow!("MXC process launch task failed"))??;

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
        })
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
) -> anyhow::Result<mxc_sdk::v1::ContainerRequest> {
    use mxc_sdk::v1::{
        ContainerRequest, FilesystemPolicy, NetworkAction, NetworkEgressPolicy,
        NetworkIngressPolicy, NetworkPolicy,
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
                default: Some(network_action),
                host_loopback: Some(network_action),
            }),
            runtime_config: None,
        }),
        working_directory: Some(path_to_string(&working_directory)),
        environment: Some(environment),
        // `shell_runtime` already supplies Haven's filtered environment. Do
        // not layer the user's ambient environment back into the sandbox.
        inherit_default_environment: Some(false),
        ..ContainerRequest::new(command_line)
    })
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
    use super::{SandboxedProcessRequest, build_container_request, quote_windows_argument};
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
        let request = build_container_request(SandboxedProcessRequest {
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
        })
        .expect("build MXC request");

        assert_eq!(request.inherit_default_environment, Some(false));
        assert_eq!(
            request.environment,
            Some(vec![(
                "PATH".to_owned(),
                "C:\\Windows\\System32".to_owned()
            )])
        );
    }
}
