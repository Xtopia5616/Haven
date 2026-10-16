//! Closed input contract for a future brokered Python Skill attempt.
//!
//! This contract intentionally describes one narrow profile: a pinned Haven
//! guest Python runtime, standard-library-only dependencies, no network, and
//! file access through the private ExecutionWorkspace snapshot. It does not
//! execute Skills; the broker must reject the profile until its image runtime,
//! input transfer, and process launcher implement and attest these guarantees.

use crate::filesystem::{DirectoryCapability, DirectoryTreeCopyLimits, DirectoryTreeCopySummary};
use std::fmt;
use std::io;
use std::path::Path;

const MAX_SKILL_ENTRY_BYTES: usize = 1024;
/// Parameter JSON travels as data, never as a command-line fragment.
pub const MAX_SKILL_PARAMETER_BYTES: usize = 64 * 1024;
const PYTHON_RUNTIME_PROFILE: &str = "haven-python-skill-v1";

/// Immutable guest runtime profile. The service image catalog must pin the
/// exact interpreter build behind this profile before it can be enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsContainerSkillRuntime {
    HavenPythonV1,
}

impl WindowsContainerSkillRuntime {
    pub fn profile(self) -> &'static str {
        match self {
            Self::HavenPythonV1 => PYTHON_RUNTIME_PROFILE,
        }
    }
}

/// Closed dependency policy for the first brokered Skill profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsContainerSkillDependencies {
    StandardLibraryOnly,
}

/// Skills receive no network capability in the initial HCS profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsContainerSkillNetworkPolicy {
    Disabled,
}

/// Model-supplied file arguments resolve only inside a private workspace
/// snapshot. The source Skill bundle is a separate read-only input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsContainerSkillFileAccess {
    ExecutionWorkspaceSnapshot,
}

/// Validated Skill process data. Parameters are redacted from Debug output
/// because they may contain user content.
pub struct WindowsContainerSkillExecutionContract {
    entry_script: String,
    parameters_json: Vec<u8>,
}

impl WindowsContainerSkillExecutionContract {
    pub fn new(entry_script: impl Into<String>, parameters_json: Vec<u8>) -> io::Result<Self> {
        let entry_script = entry_script.into();
        validate_entry_script(&entry_script)?;
        if parameters_json.is_empty() || parameters_json.len() > MAX_SKILL_PARAMETER_BYTES {
            return Err(invalid_input(
                "Skill parameter JSON is outside its byte limit",
            ));
        }
        let parameters: serde_json::Value = serde_json::from_slice(&parameters_json)
            .map_err(|_| invalid_input("Skill parameter JSON is malformed"))?;
        if !parameters.is_object() {
            return Err(invalid_input("Skill parameters must be a JSON object"));
        }
        Ok(Self {
            entry_script,
            parameters_json,
        })
    }

    pub fn entry_script(&self) -> &str {
        &self.entry_script
    }

    pub fn parameters_json(&self) -> &[u8] {
        &self.parameters_json
    }

    pub fn runtime(&self) -> WindowsContainerSkillRuntime {
        WindowsContainerSkillRuntime::HavenPythonV1
    }

    pub fn dependencies(&self) -> WindowsContainerSkillDependencies {
        WindowsContainerSkillDependencies::StandardLibraryOnly
    }

    pub fn network_policy(&self) -> WindowsContainerSkillNetworkPolicy {
        WindowsContainerSkillNetworkPolicy::Disabled
    }

    pub fn file_access(&self) -> WindowsContainerSkillFileAccess {
        WindowsContainerSkillFileAccess::ExecutionWorkspaceSnapshot
    }
}

impl fmt::Debug for WindowsContainerSkillExecutionContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WindowsContainerSkillExecutionContract")
            .field("entry_script", &self.entry_script)
            .field("parameters_json_bytes", &self.parameters_json.len())
            .field("parameters_json", &"<redacted>")
            .finish()
    }
}

/// Read-only source tree for the Skill package. The source root is never
/// returned to the provider; callers can copy only into private staging under
/// a validated tree budget.
pub struct WindowsContainerSkillBundleInput {
    source: DirectoryCapability,
}

impl WindowsContainerSkillBundleInput {
    pub fn new(source: DirectoryCapability) -> Self {
        Self { source }
    }

    pub fn copy_snapshot_to(
        &self,
        private_staging: &DirectoryCapability,
        limits: DirectoryTreeCopyLimits,
        is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeCopySummary> {
        match self.source.open_read(Path::new("requirements.txt")) {
            Ok(_) => {
                return Err(invalid_input(
                    "brokered Skills cannot declare runtime package installation",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(invalid_input(
                    "Skill dependency metadata could not be safely inspected",
                ));
            }
        }
        self.source
            .copy_tree_to(private_staging, limits, is_cancelled)
    }
}

fn validate_entry_script(path: &str) -> io::Result<()> {
    if path.is_empty()
        || path.len() > MAX_SKILL_ENTRY_BYTES
        || !path.ends_with(".py")
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return Err(invalid_input("Skill entry script path is invalid"));
    }

    for component in path.split('/') {
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
                matches!(suffix, "¹" | "²" | "³")
                    || (suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_digit())
            })
        });
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with([' ', '.'])
            || component.encode_utf16().count() > 255
            || reserved
        {
            return Err(invalid_input("Skill entry script path is invalid"));
        }
    }
    Ok(())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(
        entry: &str,
        parameters: &[u8],
    ) -> io::Result<WindowsContainerSkillExecutionContract> {
        WindowsContainerSkillExecutionContract::new(entry, parameters.to_vec())
    }

    #[test]
    fn skill_contract_is_fixed_and_redacts_parameter_contents() {
        let contract = contract("scripts/main.py", br#"{"secret":"private"}"#).unwrap();
        assert_eq!(contract.entry_script(), "scripts/main.py");
        assert_eq!(contract.parameters_json(), br#"{"secret":"private"}"#);
        assert_eq!(contract.runtime().profile(), "haven-python-skill-v1");
        assert_eq!(
            contract.dependencies(),
            WindowsContainerSkillDependencies::StandardLibraryOnly
        );
        assert_eq!(
            contract.network_policy(),
            WindowsContainerSkillNetworkPolicy::Disabled
        );
        assert_eq!(
            contract.file_access(),
            WindowsContainerSkillFileAccess::ExecutionWorkspaceSnapshot
        );
        let debug = format!("{contract:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("private"));
    }

    #[test]
    fn skill_contract_rejects_host_paths_traversal_and_reserved_names() {
        for path in [
            "",
            "main.py/../outside.py",
            "../outside.py",
            "C:/outside.py",
            "/absolute.py",
            "scripts\\main.py",
            "scripts/CON.py",
            "scripts/COM1.py",
            "scripts/trailing./main.py",
            "scripts/main.PY",
            "scripts/not-python.txt",
        ] {
            assert!(contract(path, br#"{}"#).is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn skill_contract_rejects_invalid_or_oversized_parameter_json() {
        for parameters in [b"".as_slice(), b"[]", b"null", b"{not-json}"] {
            assert!(contract("scripts/main.py", parameters).is_err());
        }
        assert!(
            contract(
                "scripts/main.py",
                &vec![b' '; MAX_SKILL_PARAMETER_BYTES + 1]
            )
            .is_err()
        );
        assert!(contract("scripts/main.py", br#"{"ok":true}"#).is_ok());
    }

    #[test]
    fn skill_bundle_is_copied_through_capabilities_and_rejects_requirements() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("scripts")).unwrap();
        std::fs::write(source.path().join("scripts/main.py"), "print('{}')").unwrap();
        let source = WindowsContainerSkillBundleInput::new(
            DirectoryCapability::open(source.path()).unwrap(),
        );
        let destination = DirectoryCapability::open(destination.path()).unwrap();
        let limits = DirectoryTreeCopyLimits::new(16, 8, 1024, 4096).unwrap();
        let summary = source
            .copy_snapshot_to(&destination, limits, || false)
            .unwrap();
        assert_eq!(summary.file_count, 1);
        assert_eq!(
            std::fs::read(destination.root().join("scripts/main.py")).unwrap(),
            b"print('{}')"
        );

        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("requirements.txt"), "unsafe-package").unwrap();
        let source = WindowsContainerSkillBundleInput::new(
            DirectoryCapability::open(source.path()).unwrap(),
        );
        let destination = tempfile::tempdir().unwrap();
        let destination = DirectoryCapability::open(destination.path()).unwrap();
        assert_eq!(
            source
                .copy_snapshot_to(&destination, limits, || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(destination.entries(16).unwrap().is_empty());
    }
}
