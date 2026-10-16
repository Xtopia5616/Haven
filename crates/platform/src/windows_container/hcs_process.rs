//! Closed HCS `ProcessParameters` construction for broker-owned guest shells.
//!
//! This builder does not start a guest process. Before quota attestation, the
//! broker may use only the fixed storage probe; caller commands remain gated.

use super::execution::{
    WindowsContainerExecutionRequest, WindowsContainerShell, WindowsContainerSkillExecutionContract,
};
use base64::Engine as _;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path};

const GUEST_WORKSPACE_INPUT_ROOT: &str = r"C:\Haven\workspace-input";
const GUEST_WORKSPACE_ROOT: &str = r"C:\Haven\workspace-output";
const GUEST_SKILL_BUNDLE_ROOT: &str = r"C:\Haven\skill-input\bundle";
const GUEST_SKILL_PARAMETERS_PATH: &str = r"C:\Haven\skill-input\parameters.json";
const GUEST_MCP_PACKAGES_ROOT: &str = r"C:\Haven\McpPackages";
const CMD_APPLICATION: &str = r"C:\Windows\System32\cmd.exe";
const POWERSHELL_APPLICATION: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
const ROBOCOPY_APPLICATION: &str = r"C:\Windows\System32\robocopy.exe";
const SKILL_PYTHON_APPLICATION: &str = r"C:\Haven\Python\python.exe";
const MCP_NODE_APPLICATION: &str = r"C:\Haven\Node\node.exe";
const FIXED_PATH: &str = r"C:\Windows\System32;C:\Windows;C:\Windows\System32\Wbem";
const MAX_GUEST_COMMAND_LINE_UNITS: usize = 32_766;
const SKILL_PYTHON_LAUNCHER: &str = r#"import io,pathlib,sys
source_path,parameters_path=sys.argv[1:3]
sys.argv=[source_path]
source=pathlib.Path(source_path)
sys.path.insert(0,str(source.parent))
sys.stdin=io.TextIOWrapper(io.BytesIO(pathlib.Path(parameters_path).read_bytes()),encoding='utf-8')
namespace=sys.modules['__main__'].__dict__
namespace.update({'__file__':source_path,'__package__':None,'__cached__':None,'__spec__':None,'__loader__':None})
code=compile(source.read_bytes(),source_path,'exec')
exec(code,namespace,namespace)"#;

const WORKSPACE_SEED_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'; $source = '{input_root}'; $destination = '{output_root}'; if (Test-Path -LiteralPath $destination) { exit 90 }; & '{robocopy_application}' $source $destination /E /COPY:DAT /DCOPY:DAT /R:0 /W:0 /XJ /NFL /NDL /NJH /NJS /NP /NC /NS; if ($LASTEXITCODE -ge 8) { exit 8 }; exit 0"#;
const STORAGE_QUOTA_PROBE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$quota = {quota_bytes}
$maximum_write = $quota + 1048576L
$written = 0L
$stream = $null
$random = $null
$skill_runtime_attested = 0
$mcp_node_runtime_attested = 0
$mcp_package_attestations = [System.Collections.Generic.List[string]]::new()
$mcp_package_profiles = @(
{mcp_package_profiles}
)
{skill_runtime_probe}
{mcp_node_runtime_probe}
. {mcp_package_tree_function}
function Test-HavenMcpPackageTree([string]$PackageId, [string]$Entrypoint, [string]$ExpectedTreeHash) {
    try {
        $package_root = Join-Path '{GUEST_MCP_PACKAGES_ROOT}' $PackageId
        $package_collection_root = [System.IO.Directory]::GetParent($package_root).FullName
        $haven_root = [System.IO.Directory]::GetParent($package_collection_root).FullName
        $drive_root = [System.IO.Path]::GetPathRoot($package_root)
        foreach ($ancestor in @($package_collection_root, $haven_root, $drive_root)) {
            if (Test-Path -LiteralPath (Join-Path $ancestor 'node_modules')) { return $false }
        }
        $tree_hash = Get-HavenMcpPackageTreeDigest -PackageRoot $package_root -Entrypoint $Entrypoint
        return [string]::Equals($tree_hash, $ExpectedTreeHash, [System.StringComparison]::Ordinal)
    } catch {
        return $false
    }
}
foreach ($package_profile in $mcp_package_profiles) {
    $package_id, $package_entrypoint, $expected_tree_hash = $package_profile.Split([char]'|')
    $package_attested = 0
    if (Test-HavenMcpPackageTree $package_id $package_entrypoint $expected_tree_hash) { $package_attested = 1 }
    $mcp_package_attestations.Add(('HAVEN_MCP_PACKAGE_ATTESTED_V2:{0}:{1}' -f $package_id, $package_attested))
}
try {
    $random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $buffer = [byte[]]::new(1048576)
    $stream = [System.IO.FileStream]::new(
        'C:\Windows\Temp\haven-storage-quota-probe.bin',
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None,
        1048576,
        [System.IO.FileOptions]::SequentialScan
    )
    while ($written -lt $maximum_write) {
        $random.GetBytes($buffer)
        $stream.Write($buffer, 0, $buffer.Length)
        $stream.Flush($true)
        $written += [long]$buffer.Length
    }
    [Console]::Error.WriteLine('storage quota was not enforced')
    exit 91
} catch [System.IO.IOException] {
    $native_code = $_.Exception.HResult -band 0xffff
    if (($native_code -ne 39) -and ($native_code -ne 112)) {
        [Console]::Error.WriteLine('storage probe failed for an unexpected I/O reason')
        exit 92
    }
    [Console]::Out.WriteLine(('HAVEN_STORAGE_QUOTA_ATTESTED_V4:{0}:{1}:{2}' -f $written, $skill_runtime_attested, $mcp_node_runtime_attested))
    foreach ($package_attestation in $mcp_package_attestations) { [Console]::Out.WriteLine($package_attestation) }
    exit 0
} finally {
    if ($null -ne $stream) { $stream.Dispose() }
    if ($null -ne $random) { $random.Dispose() }
}
"#;

/// Broker-owned process parameters. Every executable, environment key, pipe
/// option, and guest path is fixed or derived from the validated request.
pub(super) struct HcsGuestProcessParameters {
    document: ProcessParametersDocument,
}

impl HcsGuestProcessParameters {
    pub(super) fn creates_stdin_pipe(&self) -> bool {
        self.document.create_std_in_pipe
    }

    /// Copy the immutable input snapshot into the container's writable C:
    /// layer before executing user code. The process is fixed and never
    /// incorporates user command text.
    pub(super) fn workspace_seed() -> io::Result<Self> {
        let script = WORKSPACE_SEED_SCRIPT
            .replace("{input_root}", GUEST_WORKSPACE_INPUT_ROOT)
            .replace("{output_root}", GUEST_WORKSPACE_ROOT)
            .replace("{robocopy_application}", ROBOCOPY_APPLICATION);
        Ok(Self {
            document: ProcessParametersDocument {
                application_name: POWERSHELL_APPLICATION,
                command_line: powershell_command_line(&script)?,
                working_directory: r"C:\Windows\Temp".to_owned(),
                environment: fixed_environment(),
                restricted_token: true,
                emulate_console: false,
                create_std_in_pipe: false,
                create_std_out_pipe: true,
                create_std_err_pipe: true,
                use_existing_login: false,
            },
        })
    }

    /// Build the broker's fixed, non-user-controlled disk-quota probe.
    pub(super) fn quota_probe(
        storage_quota_bytes: u64,
        skill_python_runtime: Option<&super::execution::image_catalog::SkillPythonRuntimeProfile>,
        mcp_node_runtime: Option<&super::execution::image_catalog::McpNodeRuntimeProfile>,
        mcp_node_packages: &[super::execution::image_catalog::McpNodePackageProfile],
    ) -> io::Result<Self> {
        if storage_quota_bytes != super::execution::WORKSPACE_WRITE_STORAGE_QUOTA_BYTES {
            return Err(invalid_process_parameters(
                "storage quota probe only supports the fixed WorkspaceWrite ceiling",
            ));
        }
        let runtime_probe = skill_python_runtime
            .map(|runtime| {
                skill_python_runtime_probe_script(runtime.version(), runtime.executable_sha256())
            })
            .unwrap_or_default();
        let mcp_node_runtime_probe = mcp_node_runtime
            .map(|runtime| {
                mcp_node_runtime_probe_script(runtime.version(), runtime.executable_sha256())
            })
            .unwrap_or_default();
        let mcp_package_profiles = mcp_node_packages
            .iter()
            .map(mcp_node_package_catalog_entry)
            .collect::<Vec<_>>()
            .join(",\n");
        let script = STORAGE_QUOTA_PROBE_SCRIPT
            .replace("{quota_bytes}", &storage_quota_bytes.to_string())
            .replace("{GUEST_MCP_PACKAGES_ROOT}", GUEST_MCP_PACKAGES_ROOT)
            .replace("{skill_runtime_probe}", &runtime_probe)
            .replace("{mcp_node_runtime_probe}", &mcp_node_runtime_probe)
            .replace("{mcp_package_profiles}", &mcp_package_profiles)
            .replace(
                "{mcp_package_tree_function}",
                include_str!("mcp_package_tree.ps1"),
            );
        Ok(Self {
            document: ProcessParametersDocument {
                application_name: POWERSHELL_APPLICATION,
                command_line: powershell_command_line(&script)?,
                working_directory: r"C:\Windows\Temp".to_owned(),
                environment: fixed_environment(),
                restricted_token: true,
                emulate_console: false,
                create_std_in_pipe: false,
                create_std_out_pipe: true,
                create_std_err_pipe: true,
                use_existing_login: false,
            },
        })
    }

    pub(super) fn from_request(request: &WindowsContainerExecutionRequest) -> io::Result<Self> {
        let (application_name, command_line) = match request.shell() {
            WindowsContainerShell::Cmd => (
                CMD_APPLICATION,
                windows_command_line(CMD_APPLICATION, &["/d", "/s", "/c", request.command()])?,
            ),
            WindowsContainerShell::PowerShell => (
                POWERSHELL_APPLICATION,
                powershell_command_line(request.command())?,
            ),
        };
        let working_directory = guest_working_directory(request.working_directory())?;
        Ok(Self {
            document: ProcessParametersDocument {
                application_name,
                command_line,
                working_directory,
                environment: fixed_environment(),
                restricted_token: true,
                emulate_console: false,
                create_std_in_pipe: false,
                create_std_out_pipe: true,
                create_std_err_pipe: true,
                use_existing_login: false,
            },
        })
    }

    pub(super) fn from_skill_contract(
        contract: &WindowsContainerSkillExecutionContract,
    ) -> io::Result<Self> {
        let entry_script = contract.entry_script().replace('/', "\\");
        let guest_entry = format!("{GUEST_SKILL_BUNDLE_ROOT}\\{entry_script}");
        let launcher = format!(
            "exec(__import__('base64').b64decode('{}'))",
            base64::engine::general_purpose::STANDARD.encode(SKILL_PYTHON_LAUNCHER.as_bytes())
        );
        let command_line = windows_command_line(
            SKILL_PYTHON_APPLICATION,
            &[
                "-I",
                "-S",
                "-B",
                "-c",
                &launcher,
                &guest_entry,
                GUEST_SKILL_PARAMETERS_PATH,
            ],
        )?;
        Ok(Self {
            document: ProcessParametersDocument {
                application_name: SKILL_PYTHON_APPLICATION,
                command_line,
                working_directory: GUEST_WORKSPACE_ROOT.to_owned(),
                environment: fixed_environment(),
                restricted_token: true,
                emulate_console: false,
                create_std_in_pipe: false,
                create_std_out_pipe: true,
                create_std_err_pipe: true,
                use_existing_login: false,
            },
        })
    }

    /// Build a fixed Node invocation for one administrator-pinned MCP package.
    /// The process receives no workspace mapping, ambient environment, or
    /// caller-selected executable/arguments.
    pub(super) fn from_mcp_package(
        package_proof: &super::execution::McpNodePackageProof,
    ) -> io::Result<Self> {
        let package = package_proof.package();
        let package_root = format!("{GUEST_MCP_PACKAGES_ROOT}\\{}", package.id());
        let guest_entrypoint = format!("{package_root}\\{}", package.entrypoint());
        let command_line = windows_command_line(MCP_NODE_APPLICATION, &[&guest_entrypoint])?;
        Ok(Self {
            document: ProcessParametersDocument {
                application_name: MCP_NODE_APPLICATION,
                command_line,
                working_directory: package_root,
                environment: fixed_environment(),
                restricted_token: true,
                emulate_console: false,
                create_std_in_pipe: true,
                create_std_out_pipe: true,
                create_std_err_pipe: true,
                use_existing_login: false,
            },
        })
    }

    pub(super) fn to_json(&self) -> io::Result<String> {
        serde_json::to_string(&self.document).map_err(|_| {
            invalid_process_parameters("HCS process parameters could not be serialized")
        })
    }
}

fn skill_python_runtime_probe_script(version: &str, executable_sha256: &[u8; 32]) -> String {
    let digest = executable_sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        r#"try {{
    $python_path = '{SKILL_PYTHON_APPLICATION}'
    $python_hash = (Get-FileHash -LiteralPath $python_path -Algorithm SHA256).Hash.ToLowerInvariant()
    $python_version = (& $python_path --version 2>$null | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $python_version -ceq 'Python {version}' -and $python_hash -ceq '{digest}') {{
        $skill_runtime_attested = 1
    }}
}} catch {{
    $skill_runtime_attested = 0
}}"#,
    )
}

fn mcp_node_runtime_probe_script(version: &str, executable_sha256: &[u8; 32]) -> String {
    let digest = executable_sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        r#"try {{
    $node_path = '{MCP_NODE_APPLICATION}'
    $node_hash = (Get-FileHash -LiteralPath $node_path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($node_hash -ceq '{digest}') {{
        $node_version = (& $node_path --version 2>$null | Out-String).Trim()
        if ($LASTEXITCODE -eq 0 -and $node_version -ceq 'v{version}') {{
            $module_global_paths = (& $node_path -p "require('module').globalPaths.join('\n')" 2>$null | Out-String).Trim()
            if ($LASTEXITCODE -eq 0) {{
                $module_globals_safe = $true
                foreach ($module_path in ($module_global_paths -split '\r?\n')) {{
                    if (-not [string]::IsNullOrWhiteSpace($module_path) -and (Test-Path -LiteralPath $module_path -PathType Container)) {{
                        $module_globals_safe = $false
                    }}
                }}
                if ($module_globals_safe) {{ $mcp_node_runtime_attested = 1 }}
            }}
        }}
    }}
}} catch {{
    $mcp_node_runtime_attested = 0
}}"#,
    )
}

fn mcp_node_package_catalog_entry(
    package: &super::execution::image_catalog::McpNodePackageProfile,
) -> String {
    let digest = package
        .tree_sha256()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("'{}|{}|{}'", package.id(), package.entrypoint(), digest)
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessParametersDocument {
    #[serde(rename = "ApplicationName")]
    application_name: &'static str,
    #[serde(rename = "CommandLine")]
    command_line: String,
    #[serde(rename = "WorkingDirectory")]
    working_directory: String,
    #[serde(rename = "Environment")]
    environment: BTreeMap<&'static str, &'static str>,
    #[serde(rename = "RestrictedToken")]
    restricted_token: bool,
    #[serde(rename = "EmulateConsole")]
    emulate_console: bool,
    #[serde(rename = "CreateStdInPipe")]
    create_std_in_pipe: bool,
    #[serde(rename = "CreateStdOutPipe")]
    create_std_out_pipe: bool,
    #[serde(rename = "CreateStdErrPipe")]
    create_std_err_pipe: bool,
    #[serde(rename = "UseExistingLogin")]
    use_existing_login: bool,
}

fn fixed_environment() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("ComSpec", CMD_APPLICATION),
        ("PATH", FIXED_PATH),
        ("SystemRoot", r"C:\Windows"),
        ("TEMP", r"C:\Windows\Temp"),
        ("TMP", r"C:\Windows\Temp"),
        ("WINDIR", r"C:\Windows"),
    ])
}

fn guest_working_directory(relative: &Path) -> io::Result<String> {
    let mut path = GUEST_WORKSPACE_ROOT.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid_process_parameters(
                "guest working directory must stay beneath the fixed workspace root",
            ));
        };
        let name = name.to_str().ok_or_else(|| {
            invalid_process_parameters("guest working directory is not valid Unicode")
        })?;
        validate_guest_path_component(name)?;
        path.push('\\');
        path.push_str(name);
    }
    Ok(path)
}

pub(super) fn validate_guest_path_component(component: &str) -> io::Result<()> {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
    });
    if component.is_empty()
        || component.encode_utf16().count() > 255
        || matches!(component, "." | "..")
        || component.ends_with(['.', ' '])
        || component.chars().any(|character| {
            matches!(
                character,
                '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\'
            ) || character.is_control()
        })
        || reserved
    {
        return Err(invalid_process_parameters(
            "guest working directory contains an unsupported Windows path component",
        ));
    }
    Ok(())
}

fn powershell_command_line(command: &str) -> io::Result<String> {
    let mut utf16le = Vec::with_capacity(command.encode_utf16().count() * 2);
    for unit in command.encode_utf16() {
        utf16le.extend_from_slice(&unit.to_le_bytes());
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16le);
    windows_command_line(
        POWERSHELL_APPLICATION,
        &[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ],
    )
}

fn windows_command_line(program: &str, arguments: &[&str]) -> io::Result<String> {
    let mut units = Vec::new();
    quote_windows_argument(program, &mut units);
    for argument in arguments {
        units.push(b' ' as u16);
        quote_windows_argument(argument, &mut units);
    }
    if units.len() >= MAX_GUEST_COMMAND_LINE_UNITS {
        return Err(invalid_process_parameters(
            "HCS guest command line exceeds the Windows limit",
        ));
    }
    String::from_utf16(&units)
        .map_err(|_| invalid_process_parameters("HCS command line is not valid UTF-16"))
}

fn quote_windows_argument(argument: &str, output: &mut Vec<u16>) {
    output.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in argument.encode_utf16() {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else if unit == b'"' as u16 {
            output.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            output.push(unit);
            backslashes = 0;
        } else {
            output.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            output.push(unit);
            backslashes = 0;
        }
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    output.push(b'"' as u16);
}

fn invalid_process_parameters(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::{DirectoryTreeCopyLimits, DirectoryTreeDiffLimits};
    use serde_json::Value;
    use std::time::Duration;

    fn request(
        shell: WindowsContainerShell,
        command: &str,
        cwd: &str,
    ) -> WindowsContainerExecutionRequest {
        let copy = DirectoryTreeCopyLimits::new(32, 8, 1024 * 1024, 4 * 1024 * 1024)
            .expect("bounded workspace input limits");
        let diff = DirectoryTreeDiffLimits::new(copy, 16, 1024 * 1024, 4 * 1024 * 1024)
            .expect("bounded output review limits");
        let limits = super::super::execution::WindowsContainerExecutionLimits::new(
            16 * 1024 * 1024,
            Duration::from_secs(10),
            1024,
            1024,
            copy,
            diff,
        )
        .expect("execution limits are bounded");
        WindowsContainerExecutionRequest::new(shell, command, cwd, 1, 1, limits)
            .expect("test request is valid")
    }

    fn parameters(request: &WindowsContainerExecutionRequest) -> Value {
        let json = HcsGuestProcessParameters::from_request(request)
            .expect("typed request builds fixed process parameters")
            .to_json()
            .expect("process parameters serialize");
        serde_json::from_str(&json).expect("serialized parameters are valid JSON")
    }

    #[test]
    fn skill_parameters_use_pinned_python_and_keep_parameters_out_of_the_command_line() {
        let contract = WindowsContainerSkillExecutionContract::new(
            "scripts/main.py",
            br#"{"secret":"never-on-command-line"}"#.to_vec(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(
            &HcsGuestProcessParameters::from_skill_contract(&contract)
                .unwrap()
                .to_json()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["ApplicationName"], SKILL_PYTHON_APPLICATION);
        assert_eq!(value["WorkingDirectory"], GUEST_WORKSPACE_ROOT);
        assert_eq!(value["RestrictedToken"], true);
        assert_eq!(value["CreateStdInPipe"], false);
        assert_eq!(value["CreateStdOutPipe"], true);
        assert_eq!(value["CreateStdErrPipe"], true);
        let command_line = value["CommandLine"].as_str().unwrap();
        assert!(command_line.contains("-I"));
        assert!(command_line.contains("-S"));
        assert!(command_line.contains("-B"));
        assert!(command_line.contains(r"C:\Haven\skill-input\bundle\scripts\main.py"));
        assert!(command_line.contains(GUEST_SKILL_PARAMETERS_PATH));
        assert!(!command_line.contains("never-on-command-line"));
        assert!(SKILL_PYTHON_LAUNCHER.contains("sys.stdin=io.TextIOWrapper"));
        assert!(SKILL_PYTHON_LAUNCHER.contains("exec(code,namespace,namespace)"));
    }

    #[test]
    fn mcp_parameters_use_the_fixed_node_runtime_and_catalog_package_entrypoint() {
        let package = super::super::execution::image_catalog::McpNodePackageProfile::test_profile(
            "filesystem",
            "1.2.3",
            "server.mjs",
            [0xab; 32],
        );
        let package_proof = super::super::execution::test_mcp_node_package_proof(package);
        let parameters = HcsGuestProcessParameters::from_mcp_package(&package_proof)
            .expect("attested fixed package builds guest parameters");
        assert!(parameters.creates_stdin_pipe());
        let value: Value = serde_json::from_str(&parameters.to_json().unwrap()).unwrap();

        assert_eq!(value["ApplicationName"], MCP_NODE_APPLICATION);
        assert_eq!(
            value["WorkingDirectory"],
            r"C:\Haven\McpPackages\filesystem"
        );
        assert_eq!(value["RestrictedToken"], true);
        assert_eq!(value["CreateStdInPipe"], true);
        assert_eq!(value["CreateStdOutPipe"], true);
        assert_eq!(value["CreateStdErrPipe"], true);
        assert_eq!(value["UseExistingLogin"], false);
        assert_eq!(value["Environment"].as_object().unwrap().len(), 6);
        let command_line = value["CommandLine"].as_str().unwrap();
        assert!(command_line.starts_with(&format!("\"{MCP_NODE_APPLICATION}\"")));
        assert!(command_line.contains(r"C:\Haven\McpPackages\filesystem\server.mjs"));
        assert!(!command_line.contains("1.2.3"));
        assert!(!value.to_string().contains("workspace-output"));
        assert!(!value.to_string().contains("NODE_OPTIONS"));
    }

    #[test]
    fn cmd_parameters_use_fixed_guest_paths_environment_restricted_token_and_split_pipes() {
        let value = parameters(&request(
            WindowsContainerShell::Cmd,
            "echo \"hello guest\"",
            "src\\app",
        ));
        assert_eq!(value["ApplicationName"], CMD_APPLICATION);
        assert_eq!(
            value["WorkingDirectory"],
            r"C:\Haven\workspace-output\src\app"
        );
        assert_eq!(value["RestrictedToken"], true);
        assert_eq!(value["CreateStdInPipe"], false);
        assert_eq!(value["CreateStdOutPipe"], true);
        assert_eq!(value["CreateStdErrPipe"], true);
        assert_eq!(value["EmulateConsole"], false);
        assert_eq!(value["UseExistingLogin"], false);
        assert_eq!(value["Environment"]["SystemRoot"], r"C:\Windows");
        assert_eq!(value["Environment"]["PATH"], FIXED_PATH);
        assert_eq!(value["Environment"].as_object().unwrap().len(), 6);
        assert!(value["CommandLine"].as_str().unwrap().contains("/s"));
        assert!(value["CommandLine"].as_str().unwrap().contains("echo"));
        assert!(!value.to_string().contains("USERPROFILE"));
    }

    #[test]
    fn powershell_command_is_encoded_without_exposing_command_text_as_json_arguments() {
        let command = "Write-Output 'hello \"guest\" 🦀'";
        let value = parameters(&request(WindowsContainerShell::PowerShell, command, ""));
        assert_eq!(value["ApplicationName"], POWERSHELL_APPLICATION);
        let command_line = value["CommandLine"].as_str().unwrap();
        assert!(command_line.contains("-EncodedCommand"));
        assert!(!command_line.contains("hello"));

        let encoded = command_line
            .split("-EncodedCommand\" \"")
            .nth(1)
            .and_then(|value| value.strip_suffix('"'))
            .expect("encoded command is one quoted argument");
        let utf16le = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64 command is valid");
        let units = utf16le
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        assert_eq!(String::from_utf16(&units).unwrap(), command);
        assert_eq!(value["WorkingDirectory"], GUEST_WORKSPACE_ROOT);
    }

    #[test]
    fn workspace_seed_uses_fixed_paths_and_rejects_existing_destination() {
        let value: Value = serde_json::from_str(
            &HcsGuestProcessParameters::workspace_seed()
                .expect("fixed seed parameters are valid")
                .to_json()
                .expect("seed parameters serialize"),
        )
        .expect("seed parameters are valid JSON");
        assert_eq!(value["ApplicationName"], POWERSHELL_APPLICATION);
        assert_eq!(value["WorkingDirectory"], r"C:\Windows\Temp");
        assert_eq!(value["RestrictedToken"], true);
        assert_eq!(value["CreateStdInPipe"], false);
        assert_eq!(value["CreateStdOutPipe"], true);
        let command_line = value["CommandLine"].as_str().unwrap();
        assert!(command_line.contains("-EncodedCommand"));
        assert!(!command_line.contains("workspace-input"));
        assert!(!command_line.contains("workspace-output"));

        let encoded = command_line
            .split("-EncodedCommand\" \"")
            .nth(1)
            .and_then(|value| value.strip_suffix('\"'))
            .expect("seed script is encoded as one command argument");
        let utf16le = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64 seed script is valid");
        let units = utf16le
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        let script = String::from_utf16(&units).expect("seed script is UTF-16");
        assert!(script.contains("Test-Path -LiteralPath $destination"));
        assert!(script.contains(ROBOCOPY_APPLICATION));
        assert!(script.contains("/COPY:DAT"));
        assert!(script.contains("/XJ"));
        assert!(script.contains("$LASTEXITCODE -ge 8"));
        assert_eq!(GUEST_WORKSPACE_INPUT_ROOT, r"C:\Haven\workspace-input");
        assert_eq!(GUEST_WORKSPACE_ROOT, r"C:\Haven\workspace-output");
    }

    #[test]
    fn quota_probe_is_fixed_bounded_and_only_accepts_the_workspace_write_ceiling() {
        let quota = super::super::execution::WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let value: Value = serde_json::from_str(
            &HcsGuestProcessParameters::quota_probe(quota, None, None, &[])
                .expect("the fixed quota builds probe parameters")
                .to_json()
                .expect("probe parameters serialize"),
        )
        .expect("probe parameters are valid JSON");
        assert_eq!(value["ApplicationName"], POWERSHELL_APPLICATION);
        assert_eq!(value["WorkingDirectory"], r"C:\Windows\Temp");
        assert_eq!(value["RestrictedToken"], true);
        let command_line = value["CommandLine"].as_str().unwrap();
        assert!(!command_line.contains("HAVEN_STORAGE_QUOTA_ATTESTED_V4"));
        let encoded = command_line
            .split("-EncodedCommand\" \"")
            .nth(1)
            .and_then(|value| value.strip_suffix('\"'))
            .expect("probe script is encoded as one command argument");
        let utf16le = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64 probe script is valid");
        let units = utf16le
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains(&format!("$quota = {quota}\n")));
        assert!(script.contains("$maximum_write = $quota + 1048576L"));
        assert!(script.contains("RandomNumberGenerator]::Create()"));
        assert!(script.contains(r"C:\Windows\Temp\haven-storage-quota-probe.bin"));
        assert!(script.contains("$native_code -ne 39") && script.contains("$native_code -ne 112"));
        assert!(script.contains("HAVEN_STORAGE_QUOTA_ATTESTED_V4:{0}:{1}:{2}"));
        assert!(!script.contains(SKILL_PYTHON_APPLICATION));
        assert!(!script.contains(MCP_NODE_APPLICATION));
        assert!(
            HcsGuestProcessParameters::quota_probe(quota + 512, None, None, &[]).is_err(),
            "the attestation probe cannot be sized by a caller"
        );
    }

    #[test]
    fn quota_probe_checks_bounded_catalog_package_trees_with_fixed_module_roots() {
        let quota = super::super::execution::WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let package = super::super::execution::image_catalog::McpNodePackageProfile::test_profile(
            "filesystem",
            "1.2.3",
            "server.mjs",
            [0xab; 32],
        );
        let parameters = HcsGuestProcessParameters::quota_probe(
            quota,
            None,
            None,
            std::slice::from_ref(&package),
        )
        .expect("a catalog package can be checked by the fixed probe");
        let value: Value = serde_json::from_str(&parameters.to_json().unwrap()).unwrap();
        let command_line = value["CommandLine"].as_str().unwrap();
        assert!(command_line.encode_utf16().count() <= MAX_GUEST_COMMAND_LINE_UNITS);
        let encoded = command_line
            .split("-EncodedCommand\" \"")
            .nth(1)
            .and_then(|value| value.strip_suffix('\"'))
            .expect("probe script is encoded as one command argument");
        let utf16le = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64 probe script is valid");
        let units = utf16le
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains("$package_root = Join-Path 'C:\\Haven\\McpPackages' $PackageId"));
        assert!(script.contains("'filesystem|server.mjs|"));
        assert!(script.contains("HAVEN_MCP_PACKAGE_ATTESTED_V2:{0}:{1}"));
        assert!(script.contains(&"ab".repeat(32)));
        assert!(script.contains("Get-FileHash -LiteralPath $fullPath -Algorithm SHA256"));
        assert!(script.contains(
            "System.Array]::Sort($orderedRecords, [System.StringComparer]::OrdinalIgnoreCase)"
        ));
        assert!(script.contains("function Get-HavenMcpPackageTreeDigest"));
        assert!(script.contains(
            "foreach ($ancestor in @($package_collection_root, $haven_root, $drive_root))"
        ));
        assert!(script.contains("$maxEntries = 8192"));
        assert!(script.contains("$maxFiles = 4096"));
        assert!(script.contains("$maxRelativePathUnits = 1024"));
        assert!(script.contains("$maxPackageBytes = 128MB"));
        assert!(script.contains("$file.Length -gt ($maxPackageBytes - $totalBytes)"));
        assert!(!script.contains("filesystem\\1.2.3"));

        let packages = (0..super::super::execution::image_catalog::MAX_MCP_NODE_PACKAGES)
            .map(|index| {
                super::super::execution::image_catalog::McpNodePackageProfile::test_profile(
                    &format!("p{index}-{}", "x".repeat(60)),
                    "1.2.3",
                    &format!("{}.mjs", "a".repeat(124)),
                    [0xcd; 32],
                )
            })
            .collect::<Vec<_>>();
        HcsGuestProcessParameters::quota_probe(quota, None, None, &packages)
            .expect("the maximum catalog package set stays below the HCS command-line limit");
    }

    #[test]
    fn python_runtime_probe_uses_only_the_fixed_executable_and_exact_version_and_digest() {
        let script = skill_python_runtime_probe_script(
            "3.13.2",
            &[
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
                0x89, 0xab, 0xcd, 0xef,
            ],
        );
        assert!(script.contains(SKILL_PYTHON_APPLICATION));
        assert!(script.contains("Python 3.13.2"));
        assert!(
            script.contains("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert!(script.contains("$LASTEXITCODE -eq 0"));
        assert!(script.contains("$skill_runtime_attested = 1"));
    }

    #[test]
    fn mcp_node_runtime_probe_hashes_before_execution_and_rejects_global_modules() {
        let script = mcp_node_runtime_probe_script(
            super::super::execution::MCP_NODE_RUNTIME_VERSION,
            &[
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
                0x89, 0xab, 0xcd, 0xef,
            ],
        );
        assert!(script.contains(MCP_NODE_APPLICATION));
        assert!(script.contains("v24.21.0"));
        assert!(
            script.contains("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert!(script.contains("$LASTEXITCODE -eq 0"));
        assert!(script.contains("require('module').globalPaths.join"));
        assert!(script.contains("Test-Path -LiteralPath $module_path -PathType Container"));
        let digest_gate = script.find("if ($node_hash -ceq").unwrap();
        let version_execution = script
            .find("$node_version = (& $node_path --version")
            .unwrap();
        assert!(digest_gate < version_execution);
        assert!(script.contains("$mcp_node_runtime_attested = 1"));
    }

    #[test]
    fn guest_working_directory_rejects_aliases_and_invalid_unicode_components() {
        for path in [
            Path::new("..\\outside"),
            Path::new("CON"),
            Path::new("trailing."),
            Path::new("bad:name"),
            Path::new("bad?name"),
        ] {
            assert!(guest_working_directory(path).is_err(), "path {path:?}");
        }
    }

    #[test]
    fn encoded_powershell_command_must_fit_the_guest_windows_command_line_limit() {
        let command = "x".repeat(20_000);
        let request = request(WindowsContainerShell::PowerShell, &command, "");
        assert!(HcsGuestProcessParameters::from_request(&request).is_err());
    }
}
