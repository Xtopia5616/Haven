use haven_platform::sandboxed_process::{SandboxedProcessNetworkPolicy, SandboxedProcessRequest};
use std::path::PathBuf;
use std::process::Command;

/// Copy a fully prepared child command into the platform sandbox request.
/// Security scopes remain owned and compiled by the caller's live policy
/// permit; this adapter only preserves the exact command and environment.
pub(crate) fn request_from_command(
    command: &Command,
    readwrite_paths: Vec<PathBuf>,
    readonly_paths: Vec<PathBuf>,
    protected_readonly_paths: Vec<PathBuf>,
    denied_paths: Vec<PathBuf>,
    network_policy: SandboxedProcessNetworkPolicy,
    allow_desktop_handle_access: bool,
) -> anyhow::Result<SandboxedProcessRequest> {
    let working_directory = command
        .get_current_dir()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let environment = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_os_string(), value.to_os_string())))
        .collect();
    Ok(SandboxedProcessRequest {
        program: command.get_program().to_os_string(),
        arguments: command
            .get_args()
            .map(|argument| argument.to_os_string())
            .collect(),
        working_directory,
        readwrite_paths,
        readonly_paths,
        protected_readonly_paths,
        denied_paths,
        environment,
        network_policy,
        allow_desktop_handle_access,
    })
}

#[cfg(windows)]
pub(crate) fn runtime_dependency_roots(command: &Command) -> Vec<PathBuf> {
    let environment = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_os_string(), value.to_os_string())))
        .collect::<Vec<_>>();
    let mut roots = Vec::new();
    if let Some(system_root) = environment
        .iter()
        .find(|(key, _)| {
            key.to_str()
                .is_some_and(|key| key.eq_ignore_ascii_case("SYSTEMROOT"))
        })
        .map(|(_, value)| PathBuf::from(value))
    {
        roots.push(system_root.clone());
        roots.push(system_root.join("System32"));
        roots.push(system_root.join(r"System32\WindowsPowerShell\v1.0"));
    }
    roots.extend(
        environment
            .iter()
            .find(|(key, _)| {
                key.to_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case("PATH"))
            })
            .map(|(_, value)| std::env::split_paths(value).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    roots.retain(|path| path.is_dir());
    roots
}
