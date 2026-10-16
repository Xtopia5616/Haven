//! Fixed, administrator-provisioned source for the broker's HCS image layers.
//!
//! Haven does not download or select images per request. The broker reads one
//! strict catalog beneath its protected Program Files root and pins every
//! listed layer directory for the catalog lifetime.

use super::WindowsContainerSkillRuntime;
use super::broker_workspace::{BrokerServiceDirectoryAccess, install_protected_directory};
use super::named_pipe::broker_service_sid_string;
use crate::filesystem::{DirectoryCapability, DirectoryEntryKind};
use crate::windows_container::WindowsContainerLayer;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::Path;

const IMAGE_CATALOG_DIRECTORY: &str = "ContainerImages";
const LAYERS_DIRECTORY: &str = "layers";
const CATALOG_FILE: &str = "catalog.json";
const CATALOG_VERSION: u32 = 4;
const MAX_CATALOG_BYTES: u64 = 64 * 1024;
const MAX_IMAGE_LAYERS: usize = 64;
const MAX_LAYER_DIRECTORY_NAME: usize = 64;
const MAX_MCP_NODE_PACKAGES: usize = 64;
const MAX_MCP_PACKAGE_ID_BYTES: usize = 64;
const MAX_MCP_PACKAGE_ENTRYPOINT_BYTES: usize = 128;

/// One fixed, trusted image catalog loaded from the broker installation root.
/// The layer directories stay pinned while this value is alive.
#[derive(Clone)]
pub(crate) struct BrokerContainerImageCatalog {
    layers: Vec<WindowsContainerLayer>,
    skill_python_runtime: Option<SkillPythonRuntimeProfile>,
    mcp_node_runtime: Option<McpNodeRuntimeProfile>,
    // Staged for the brokered MCP launcher; no execution path consumes it yet.
    #[allow(dead_code)]
    mcp_node_packages: Vec<McpNodePackageProfile>,
}

/// Admin-pinned Python identity for the fixed guest executable path. This is
/// metadata only; a successful startup probe must still attest it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkillPythonRuntimeProfile {
    version: String,
    executable_sha256: [u8; 32],
}

impl SkillPythonRuntimeProfile {
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn executable_sha256(&self) -> &[u8; 32] {
        &self.executable_sha256
    }
}

/// Admin-pinned Node identity for the first brokered MCP runtime profile.
/// This is catalog metadata only; startup must attest the guest executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpNodeRuntimeProfile {
    version: String,
    executable_sha256: [u8; 32],
}

impl McpNodeRuntimeProfile {
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn executable_sha256(&self) -> &[u8; 32] {
        &self.executable_sha256
    }
}

/// Administrator-pinned Node package identity baked into the protected image.
/// The entrypoint is a leaf `.mjs` filename beneath the package's fixed guest
/// directory; this descriptor never carries a host path or process command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpNodePackageProfile {
    id: String,
    version: String,
    entrypoint: String,
}

// The next brokered MCP session slice will consume these validated fields.
#[allow(dead_code)]
impl McpNodePackageProfile {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn entrypoint(&self) -> &str {
        &self.entrypoint
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogDocument {
    version: u32,
    layers: Vec<CatalogLayer>,
    #[serde(default)]
    skill_python_runtime: Option<CatalogSkillPythonRuntime>,
    #[serde(default)]
    mcp_node_runtime: Option<CatalogMcpNodeRuntime>,
    #[serde(default)]
    mcp_node_packages: Vec<CatalogMcpNodePackage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogLayer {
    id: String,
    directory: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogSkillPythonRuntime {
    profile: String,
    version: String,
    executable_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogMcpNodeRuntime {
    profile: String,
    version: String,
    executable_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogMcpNodePackage {
    id: String,
    version: String,
    entrypoint: String,
}

impl BrokerContainerImageCatalog {
    /// Create the fixed catalog directory during explicit elevated install.
    /// Administrators provision the licensed image and catalog beneath it.
    pub(crate) fn install(program_files: &Path) -> io::Result<()> {
        let haven_directory = program_files.join("Haven");
        DirectoryCapability::open(&haven_directory)?;
        let image_directory = haven_directory.join(IMAGE_CATALOG_DIRECTORY);
        let service_sid = broker_service_sid_string()?;
        install_protected_directory(
            &image_directory,
            &service_sid,
            BrokerServiceDirectoryAccess::ReadOnly,
        )
    }

    /// Load the single service-selected image catalog. Missing or malformed
    /// metadata prevents the broker from starting; requests cannot name an
    /// alternate catalog or layer path.
    pub(crate) fn open(program_files: &Path) -> io::Result<Self> {
        let catalog_path = program_files.join("Haven").join(IMAGE_CATALOG_DIRECTORY);
        let root = DirectoryCapability::open(&catalog_path)?;
        Self::load(root)
    }

    pub(crate) fn sandbox_config(
        &self,
        name: impl Into<String>,
        storage_sandbox_size_bytes: u64,
    ) -> io::Result<super::super::WindowsContainerSandboxConfig> {
        super::super::WindowsContainerSandboxConfig::new(
            name,
            storage_sandbox_size_bytes,
            self.layers.clone(),
        )
    }

    pub(crate) fn skill_python_runtime(&self) -> Option<&SkillPythonRuntimeProfile> {
        self.skill_python_runtime.as_ref()
    }

    pub(crate) fn mcp_node_runtime(&self) -> Option<&McpNodeRuntimeProfile> {
        self.mcp_node_runtime.as_ref()
    }

    #[allow(dead_code)] // Consumed when the brokered MCP launcher is wired.
    pub(crate) fn mcp_node_package(&self, id: &str) -> Option<&McpNodePackageProfile> {
        self.mcp_node_packages
            .iter()
            .find(|package| package.id == id)
    }

    fn load(root: DirectoryCapability) -> io::Result<Self> {
        let mut manifest = root.open_read(Path::new(CATALOG_FILE))?;
        let manifest_metadata = manifest.metadata()?;
        if !manifest_metadata.is_file()
            || manifest_metadata.len() == 0
            || manifest_metadata.len() > MAX_CATALOG_BYTES
        {
            return Err(invalid_data("container image catalog size is invalid"));
        }

        let mut bytes = Vec::with_capacity(manifest_metadata.len() as usize);
        (&mut manifest)
            .take(MAX_CATALOG_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CATALOG_BYTES {
            return Err(invalid_data(
                "container image catalog exceeds its byte limit",
            ));
        }
        let document: CatalogDocument = serde_json::from_slice(&bytes)
            .map_err(|_| invalid_data("container image catalog is malformed"))?;
        if document.version != CATALOG_VERSION
            || document.layers.is_empty()
            || document.layers.len() > MAX_IMAGE_LAYERS
        {
            return Err(invalid_data(
                "container image catalog version or layer count is unsupported",
            ));
        }

        let skill_python_runtime = document
            .skill_python_runtime
            .map(parse_skill_python_runtime)
            .transpose()?;
        let mcp_node_runtime = document
            .mcp_node_runtime
            .map(parse_mcp_node_runtime)
            .transpose()?;
        if document.mcp_node_packages.len() > MAX_MCP_NODE_PACKAGES
            || (!document.mcp_node_packages.is_empty() && mcp_node_runtime.is_none())
        {
            return Err(invalid_data(
                "container MCP package count or runtime dependency is invalid",
            ));
        }
        let mut seen_mcp_package_ids = BTreeSet::new();
        let mcp_node_packages = document
            .mcp_node_packages
            .into_iter()
            .map(parse_mcp_node_package)
            .map(|package| {
                let package = package?;
                if !seen_mcp_package_ids.insert(package.id.to_ascii_lowercase()) {
                    return Err(invalid_data("container MCP package ID is duplicated"));
                }
                Ok(package)
            })
            .collect::<io::Result<Vec<_>>>()?;

        let layers_root_path = root.root().join(LAYERS_DIRECTORY);
        let layers_root = DirectoryCapability::open(&layers_root_path)?;
        let disk_entries = layers_root.entries(MAX_IMAGE_LAYERS)?;
        if disk_entries.len() != document.layers.len()
            || disk_entries.iter().any(|entry| {
                entry.kind != DirectoryEntryKind::Directory
                    || entry
                        .name
                        .to_str()
                        .is_none_or(|name| !valid_layer_directory_name(name))
            })
        {
            return Err(invalid_data(
                "container image layer directory set does not match its catalog",
            ));
        }

        let disk_names: BTreeSet<String> = disk_entries
            .into_iter()
            .filter_map(|entry| entry.name.to_str().map(str::to_ascii_lowercase))
            .collect();
        let mut seen_ids = BTreeSet::new();
        let mut seen_directories = BTreeSet::new();
        let mut layers = Vec::with_capacity(document.layers.len());
        for record in document.layers {
            if record.id.is_empty()
                || record.id.len() > 128
                || record.id.chars().any(char::is_control)
                || !seen_ids.insert(record.id.to_ascii_lowercase())
                || !valid_layer_directory_name(&record.directory)
                || !seen_directories.insert(record.directory.to_ascii_lowercase())
                || !disk_names.contains(&record.directory.to_ascii_lowercase())
            {
                return Err(invalid_data("container image layer entry is invalid"));
            }

            let directory = DirectoryCapability::open(&layers_root.root().join(&record.directory))?;
            layers.push(WindowsContainerLayer::from_trusted_catalog(
                record.id, directory,
            )?);
        }

        Ok(Self {
            layers,
            skill_python_runtime,
            mcp_node_runtime,
            mcp_node_packages,
        })
    }
}

fn parse_skill_python_runtime(
    runtime: CatalogSkillPythonRuntime,
) -> io::Result<SkillPythonRuntimeProfile> {
    if runtime.profile != WindowsContainerSkillRuntime::HavenPythonV1.profile()
        || !valid_python_version(&runtime.version)
    {
        return Err(invalid_data(
            "container Skill Python runtime profile is invalid",
        ));
    }
    let executable_sha256 = parse_sha256(&runtime.executable_sha256)?;
    Ok(SkillPythonRuntimeProfile {
        version: runtime.version,
        executable_sha256,
    })
}

fn parse_mcp_node_runtime(runtime: CatalogMcpNodeRuntime) -> io::Result<McpNodeRuntimeProfile> {
    if runtime.profile != super::MCP_NODE_RUNTIME_PROFILE
        || runtime.version != super::MCP_NODE_RUNTIME_VERSION
    {
        return Err(invalid_data(
            "container MCP Node runtime profile is invalid",
        ));
    }
    let executable_sha256 = parse_sha256(&runtime.executable_sha256)?;
    Ok(McpNodeRuntimeProfile {
        version: runtime.version,
        executable_sha256,
    })
}

fn parse_mcp_node_package(package: CatalogMcpNodePackage) -> io::Result<McpNodePackageProfile> {
    if !valid_mcp_package_id(&package.id)
        || !valid_mcp_package_version(&package.version)
        || !valid_mcp_package_entrypoint(&package.entrypoint)
    {
        return Err(invalid_data("container MCP package profile is invalid"));
    }
    Ok(McpNodePackageProfile {
        id: package.id,
        version: package.version,
        entrypoint: package.entrypoint,
    })
}

fn valid_mcp_package_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MCP_PACKAGE_ID_BYTES
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_mcp_package_version(version: &str) -> bool {
    let components = version.split('.').collect::<Vec<_>>();
    components.len() == 3
        && components.iter().all(|component| {
            !component.is_empty()
                && component.len() <= 9
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component == &"0" || !component.starts_with('0'))
        })
}

fn valid_mcp_package_entrypoint(entrypoint: &str) -> bool {
    let Some(stem) = entrypoint.strip_suffix(".mjs") else {
        return false;
    };
    if stem.is_empty()
        || entrypoint.len() > MAX_MCP_PACKAGE_ENTRYPOINT_BYTES
        || !stem.as_bytes()[0].is_ascii_alphanumeric()
        || !entrypoint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return false;
    }
    !matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
    ) && !((stem.len() == 4)
        && (stem.to_ascii_uppercase().starts_with("COM")
            || stem.to_ascii_uppercase().starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn valid_python_version(version: &str) -> bool {
    let components = version.split('.').collect::<Vec<_>>();
    components.len() == 3
        && components[0] == "3"
        && components.iter().all(|component| {
            !component.is_empty()
                && component.len() <= 3
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component == &"0" || !component.starts_with('0'))
        })
}

fn parse_sha256(value: &str) -> io::Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_data(
            "container Skill Python executable digest is invalid",
        ));
    }
    let mut digest = [0_u8; 32];
    let (pairs, _) = value.as_bytes().as_chunks::<2>();
    for (index, pair) in pairs.iter().enumerate() {
        let hex = std::str::from_utf8(pair)
            .map_err(|_| invalid_data("container Skill Python executable digest is invalid"))?;
        digest[index] = u8::from_str_radix(hex, 16)
            .map_err(|_| invalid_data("container Skill Python executable digest is invalid"))?;
    }
    Ok(digest)
}

fn valid_layer_directory_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let reserved_device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'));
    !reserved_device
        && !name.is_empty()
        && name.len() <= MAX_LAYER_DIRECTORY_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn catalog_root(manifest: &str, directory_names: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let layers = root.path().join(LAYERS_DIRECTORY);
        fs::create_dir(&layers).unwrap();
        for name in directory_names {
            fs::create_dir(layers.join(name)).unwrap();
        }
        fs::write(root.path().join(CATALOG_FILE), manifest).unwrap();
        root
    }

    #[test]
    fn catalog_preserves_hcs_layer_order_and_builds_a_pinned_config() {
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"child-id","directory":"child"},{"id":"base-id","directory":"base"}]}"#,
            &["base", "child"],
        );
        let catalog =
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .unwrap();

        let config = catalog.sandbox_config("test-system", 1024).unwrap();
        assert_eq!(config.layers.len(), 2);
        assert_eq!(config.layers[0].id, "child-id");
        assert_eq!(config.layers[1].id, "base-id");
        assert!(config.layers.iter().all(|layer| layer._directory.is_some()));
    }

    #[test]
    fn catalog_rejects_unknown_fields_and_unsupported_versions() {
        for manifest in [
            r#"{"version":4,"layers":[{"id":"base","directory":"base","path":"C:\\evil"}]}"#,
            r#"{"version":2,"layers":[{"id":"base","directory":"base"}]}"#,
            r#"{"version":5,"layers":[{"id":"base","directory":"base"}]}"#,
            r#"{"version":1,"layers":[{"id":"base","directory":"base"}]}"#,
        ] {
            let root = catalog_root(manifest, &["base"]);
            assert!(
                BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                    .is_err()
            );
        }
    }

    #[test]
    fn catalog_rejects_traversal_duplicate_ids_and_unlisted_directories() {
        let traversal = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"../base"}]}"#,
            &["base"],
        );
        assert!(
            BrokerContainerImageCatalog::load(DirectoryCapability::open(traversal.path()).unwrap())
                .is_err()
        );

        let duplicate_ids = catalog_root(
            r#"{"version":4,"layers":[{"id":"same","directory":"base"},{"id":"SAME","directory":"child"}]}"#,
            &["base", "child"],
        );
        assert!(
            BrokerContainerImageCatalog::load(
                DirectoryCapability::open(duplicate_ids.path()).unwrap()
            )
            .is_err()
        );

        let duplicate_directories = catalog_root(
            r#"{"version":4,"layers":[{"id":"base-one","directory":"base"},{"id":"base-two","directory":"BASE"}]}"#,
            &["base", "child"],
        );
        assert!(
            BrokerContainerImageCatalog::load(
                DirectoryCapability::open(duplicate_directories.path()).unwrap()
            )
            .is_err()
        );

        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"base"}]}"#,
            &["base", "unlisted"],
        );
        assert!(
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .is_err()
        );
    }

    #[test]
    fn catalog_rejects_missing_layer_directories() {
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"missing"}]}"#,
            &["base"],
        );
        assert!(
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .is_err()
        );
    }

    #[test]
    fn layer_directory_names_reject_windows_device_aliases() {
        for name in ["CON", "aux", "COM1", "LPT9"] {
            assert!(!valid_layer_directory_name(name));
        }
        for name in ["base", "layer_2", "win-server-2025"] {
            assert!(valid_layer_directory_name(name));
        }
    }

    #[test]
    fn catalog_v4_pins_optional_skill_python_version_and_executable_digest() {
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"base"}],"skill_python_runtime":{"profile":"haven-python-skill-v1","version":"3.13.2","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}"#,
            &["base"],
        );
        let catalog =
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .unwrap();
        let runtime = catalog.skill_python_runtime().unwrap();
        assert_eq!(runtime.version(), "3.13.2");
        assert_eq!(
            runtime.executable_sha256(),
            &[
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
                0x89, 0xab, 0xcd, 0xef,
            ]
        );
    }

    #[test]
    fn catalog_rejects_unpinned_or_malformed_skill_python_profiles() {
        for runtime in [
            r#"{"profile":"other","version":"3.13.2","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            r#"{"profile":"haven-python-skill-v1","version":"3.013.2","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            r#"{"profile":"haven-python-skill-v1","version":"3.13.2","executable_sha256":"0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            r#"{"profile":"haven-python-skill-v1","version":"3.13.2","executable_sha256":"short"}"#,
            r#"{"profile":"haven-python-skill-v1","version":"3.13.2","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","application":"C:\\evil.exe"}"#,
        ] {
            let manifest = format!(
                r#"{{"version":4,"layers":[{{"id":"base","directory":"base"}}],"skill_python_runtime":{runtime}}}"#
            );
            let root = catalog_root(&manifest, &["base"]);
            assert!(
                BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                    .is_err(),
                "accepted profile {runtime}"
            );
        }
    }

    #[test]
    fn catalog_without_skill_profile_remains_shell_usable_but_has_no_skill_runtime() {
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"base"}]}"#,
            &["base"],
        );
        let catalog =
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .unwrap();
        assert!(catalog.skill_python_runtime().is_none());
    }

    #[test]
    fn catalog_v4_pins_the_fixed_mcp_node_runtime_profile() {
        assert_eq!(
            super::super::MCP_NODE_RUNTIME_VERSION,
            include_str!("../../../../../.node-version").trim()
        );
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"base"}],"mcp_node_runtime":{"profile":"haven-mcp-node-v1","version":"24.21.0","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}"#,
            &["base"],
        );
        let catalog =
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .unwrap();
        let runtime = catalog.mcp_node_runtime().unwrap();
        assert_eq!(runtime.version(), "24.21.0");
        assert_eq!(runtime.executable_sha256()[..4], [0x01, 0x23, 0x45, 0x67]);
    }

    #[test]
    fn catalog_rejects_unpinned_or_malformed_mcp_node_profiles() {
        for runtime in [
            r#"{"profile":"other","version":"24.21.0","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            r#"{"profile":"haven-mcp-node-v1","version":"24.22.0","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            r#"{"profile":"haven-mcp-node-v1","version":"24.21.0","executable_sha256":"0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            r#"{"profile":"haven-mcp-node-v1","version":"24.21.0","executable_sha256":"short"}"#,
            r#"{"profile":"haven-mcp-node-v1","version":"24.21.0","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","application":"C:\\evil.exe"}"#,
        ] {
            let manifest = format!(
                r#"{{"version":4,"layers":[{{"id":"base","directory":"base"}}],"mcp_node_runtime":{runtime}}}"#
            );
            let root = catalog_root(&manifest, &["base"]);
            assert!(
                BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                    .is_err(),
                "accepted profile {runtime}"
            );
        }
    }

    #[test]
    fn catalog_without_mcp_node_profile_has_no_runtime_capability() {
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"base"}]}"#,
            &["base"],
        );
        let catalog =
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .unwrap();
        assert!(catalog.mcp_node_runtime().is_none());
    }

    #[test]
    fn catalog_v4_maps_admin_package_id_to_a_closed_guest_entrypoint() {
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"base"}],"mcp_node_runtime":{"profile":"haven-mcp-node-v1","version":"24.21.0","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},"mcp_node_packages":[{"id":"filesystem","version":"1.2.3","entrypoint":"server.mjs"}]}"#,
            &["base"],
        );
        let catalog =
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .unwrap();

        let package = catalog.mcp_node_package("filesystem").unwrap();
        assert_eq!(package.id(), "filesystem");
        assert_eq!(package.version(), "1.2.3");
        assert_eq!(package.entrypoint(), "server.mjs");
        assert!(catalog.mcp_node_package("other").is_none());
    }

    #[test]
    fn catalog_rejects_mcp_packages_without_the_fixed_node_runtime() {
        let root = catalog_root(
            r#"{"version":4,"layers":[{"id":"base","directory":"base"}],"mcp_node_packages":[{"id":"filesystem","version":"1.2.3","entrypoint":"server.mjs"}]}"#,
            &["base"],
        );
        assert!(
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .is_err()
        );
    }

    #[test]
    fn catalog_rejects_unsafe_or_ambiguous_mcp_package_descriptors() {
        let runtime = r#"{"profile":"haven-mcp-node-v1","version":"24.21.0","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#;
        for package in [
            r#"{"id":"../filesystem","version":"1.2.3","entrypoint":"server.mjs"}"#,
            r#"{"id":"FileSystem","version":"1.2.3","entrypoint":"server.mjs"}"#,
            r#"{"id":"filesystem","version":"01.2.3","entrypoint":"server.mjs"}"#,
            r#"{"id":"filesystem","version":"1.2.3-beta","entrypoint":"server.mjs"}"#,
            r#"{"id":"filesystem","version":"1.2.3","entrypoint":"../server.mjs"}"#,
            r#"{"id":"filesystem","version":"1.2.3","entrypoint":"CON.mjs"}"#,
            r#"{"id":"filesystem","version":"1.2.3","entrypoint":"server.js"}"#,
            r#"{"id":"filesystem","version":"1.2.3","entrypoint":"server.mjs","command":"node"}"#,
        ] {
            let manifest = format!(
                r#"{{"version":4,"layers":[{{"id":"base","directory":"base"}}],"mcp_node_runtime":{runtime},"mcp_node_packages":[{package}]}}"#
            );
            let root = catalog_root(&manifest, &["base"]);
            assert!(
                BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                    .is_err(),
                "accepted MCP package profile {package}"
            );
        }
    }

    #[test]
    fn catalog_rejects_duplicate_mcp_package_ids_and_excessive_package_count() {
        let runtime = r#"{"profile":"haven-mcp-node-v1","version":"24.21.0","executable_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#;
        let duplicate = format!(
            r#"{{"version":4,"layers":[{{"id":"base","directory":"base"}}],"mcp_node_runtime":{runtime},"mcp_node_packages":[{{"id":"filesystem","version":"1.2.3","entrypoint":"server.mjs"}},{{"id":"filesystem","version":"2.0.0","entrypoint":"server.mjs"}}]}}"#
        );
        let root = catalog_root(&duplicate, &["base"]);
        assert!(
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .is_err()
        );

        let packages = (0..=MAX_MCP_NODE_PACKAGES)
            .map(|index| {
                format!(r#"{{"id":"server-{index}","version":"1.0.0","entrypoint":"server.mjs"}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        let excessive = format!(
            r#"{{"version":4,"layers":[{{"id":"base","directory":"base"}}],"mcp_node_runtime":{runtime},"mcp_node_packages":[{packages}]}}"#
        );
        let root = catalog_root(&excessive, &["base"]);
        assert!(
            BrokerContainerImageCatalog::load(DirectoryCapability::open(root.path()).unwrap())
                .is_err()
        );
    }

    #[test]
    fn mcp_package_entrypoints_are_leaf_mjs_names_and_not_windows_devices() {
        for value in ["server.mjs", "package-entry_v2.mjs"] {
            assert!(valid_mcp_package_entrypoint(value));
        }
        for value in [
            "",
            ".mjs",
            "../server.mjs",
            "folder/server.mjs",
            "folder\\server.mjs",
            "CON.mjs",
            "COM1.mjs",
            "server.js",
            "server.mjs ",
        ] {
            assert!(!valid_mcp_package_entrypoint(value), "accepted {value}");
        }
    }
}
