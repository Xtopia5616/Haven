//! Closed HCS `ProcessParameters` construction for broker-owned guest shells.
//!
//! This builder does not start a guest process. Until a writable output layer
//! is mounted and quota enforcement is attested, the broker must not execute
//! caller commands in a compute system.

use super::execution::{WindowsContainerExecutionRequest, WindowsContainerShell};
use base64::Engine as _;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path};

const GUEST_WORKSPACE_ROOT: &str = r"C:\Haven\workspace-input";
const CMD_APPLICATION: &str = r"C:\Windows\System32\cmd.exe";
const POWERSHELL_APPLICATION: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
const FIXED_PATH: &str = r"C:\Windows\System32;C:\Windows;C:\Windows\System32\Wbem";
const MAX_GUEST_COMMAND_LINE_UNITS: usize = 32_766;

/// Broker-owned process parameters. Every executable, environment key, pipe
/// option, and guest path is fixed or derived from the validated request.
pub(super) struct HcsGuestProcessParameters {
    document: ProcessParametersDocument,
}

impl HcsGuestProcessParameters {
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

    pub(super) fn to_json(&self) -> io::Result<String> {
        serde_json::to_string(&self.document).map_err(|_| {
            invalid_process_parameters("HCS process parameters could not be serialized")
        })
    }
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

fn validate_guest_path_component(component: &str) -> io::Result<()> {
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
    fn cmd_parameters_use_fixed_guest_paths_environment_restricted_token_and_split_pipes() {
        let value = parameters(&request(
            WindowsContainerShell::Cmd,
            "echo \"hello guest\"",
            "src\\app",
        ));
        assert_eq!(value["ApplicationName"], CMD_APPLICATION);
        assert_eq!(
            value["WorkingDirectory"],
            r"C:\Haven\workspace-input\src\app"
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
