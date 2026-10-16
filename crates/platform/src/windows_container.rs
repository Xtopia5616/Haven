//! Fixed HCS v1 configuration for Haven's optional Windows container backend.
//!
//! This module builds a fixed configuration document and exposes a typed
//! execution port. It does not create a container, install services, or
//! authorize a tool call. Callers must supply image layers from the broker's
//! trusted local image catalog. The HCS schema is intentionally closed: the
//! only allowed host mapping is an explicitly attached, read-only private
//! workspace input; network endpoints, credentials, devices, arbitrary mounts,
//! or caller-selected isolation mode cannot be serialized.

pub mod execution;
#[cfg(windows)]
#[expect(
    dead_code,
    reason = "bounded transport is prepared before HCS export/provider integration"
)]
mod hcs_export_transport;
#[cfg(windows)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "synchronous HCS layer export awaits isolated broker worker wiring"
    )
)]
mod hcs_layer_export;
#[cfg(windows)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the bounded legacy HCS transport extractor awaits native layer-reader wiring"
    )
)]
mod hcs_layer_output;
#[cfg(windows)]
#[expect(
    dead_code,
    reason = "guest process creation stays private until quota and output-layer handling are wired"
)]
mod hcs_process;
#[cfg(windows)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "bounded guest output polling is consumed when the HCS provider is wired"
    )
)]
mod hcs_process_output;
#[cfg(windows)]
#[expect(
    dead_code,
    reason = "guest process handles stay private until writable-layer and bounded stream wiring are complete"
)]
mod hcs_process_runtime;
#[cfg(windows)]
#[expect(
    dead_code,
    reason = "the broker keeps HCS execution unavailable until quota attestation and provider wiring are complete"
)]
mod hcs_runtime;
#[cfg(not(test))]
#[expect(
    dead_code,
    reason = "HCS scratch quota runtime gate has not passed, so no production provider may consume this prototype yet"
)]
pub(crate) mod scratch_vhd;
#[cfg(test)]
pub(crate) mod scratch_vhd;

use crate::filesystem::DirectoryCapability;
use serde::Serialize;
use std::io;
use std::path::{Path, PathBuf};

const HCS_OWNER: &str = "HavenContainerBroker";
const WORKSPACE_INPUT_CONTAINER_PATH: &str = r"C:\Haven\workspace-input";

/// One trusted, read-only parent layer from the broker's local image catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsContainerLayer {
    id: String,
    path: PathBuf,
}

impl WindowsContainerLayer {
    /// Construct a layer descriptor from broker-owned image metadata.
    pub fn new(id: impl Into<String>, path: impl Into<PathBuf>) -> io::Result<Self> {
        let id = id.into();
        let path = path.into();
        if id.is_empty() || id.chars().any(char::is_control) {
            return Err(invalid_config("container layer ID is invalid"));
        }
        validate_local_absolute_path(&path, "container layer path")?;
        Ok(Self { id, path })
    }
}

/// Closed HCS configuration builder for a single Hyper-V-isolated execution.
///
/// `storage_sandbox_size_bytes` is serialized as HCS `StorageSandboxSize` in
/// bytes and must match the formatted per-attempt writable-layer VHDX. The
/// document also carries that attempt directory as `LayerFolderPath`. A
/// successful serialization does not prove that a target host mounts or
/// enforces the VHDX capacity; runtime over-quota Windows verification remains
/// mandatory before this backend is connected to production tool execution.
#[derive(Debug, Clone)]
pub struct WindowsContainerSandboxConfig {
    name: String,
    storage_sandbox_size_bytes: u64,
    layers: Vec<WindowsContainerLayer>,
    writable_layer_folder: Option<PathBuf>,
    workspace_input: Option<DirectoryCapability>,
}

impl WindowsContainerSandboxConfig {
    pub fn new(
        name: impl Into<String>,
        storage_sandbox_size_bytes: u64,
        layers: Vec<WindowsContainerLayer>,
    ) -> io::Result<Self> {
        let name = name.into();
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err(invalid_config("container name is invalid"));
        }
        if storage_sandbox_size_bytes == 0 {
            return Err(invalid_config(
                "container storage sandbox size must be non-zero",
            ));
        }
        if layers.is_empty() {
            return Err(invalid_config(
                "container configuration requires a trusted base layer",
            ));
        }

        Ok(Self {
            name,
            storage_sandbox_size_bytes,
            layers,
            writable_layer_folder: None,
            workspace_input: None,
        })
    }

    /// Bind the per-attempt writable layer after the broker formatter has
    /// verified it. The caller must keep the layer owner alive until HCS has
    /// stopped using this path.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the broker provider will consume this binding after output extraction is implemented"
        )
    )]
    pub(crate) fn with_bounded_writable_layer(
        mut self,
        layer: &scratch_vhd::BoundedScratchVhd,
    ) -> io::Result<Self> {
        if self.writable_layer_folder.is_some() {
            return Err(invalid_config(
                "container configuration already has a writable layer",
            ));
        }
        if layer.maximum_size_bytes() != self.storage_sandbox_size_bytes {
            return Err(invalid_config(
                "writable layer capacity does not match the requested storage ceiling",
            ));
        }
        let path = layer.layer_path()?.to_path_buf();
        validate_local_absolute_path(&path, "writable layer path")?;
        self.writable_layer_folder = Some(path);
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn with_test_writable_layer_folder(mut self, path: PathBuf) -> io::Result<Self> {
        validate_local_absolute_path(&path, "test writable layer path")?;
        self.writable_layer_folder = Some(path);
        Ok(self)
    }

    #[cfg(windows)]
    fn hcs_system_id(&self) -> &str {
        &self.name
    }

    /// Attach the broker-owned, per-attempt workspace snapshot as the single
    /// read-only directory mapping visible to the guest.
    ///
    /// The capability remains owned by this config for the lifetime of the
    /// HCS document owner, pinning the directory identity while HCS consumes
    /// its path. The caller cannot choose a guest destination or writable
    /// mapping through this API.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the broker HCS provider is implemented in a later execution-workspace slice"
        )
    )]
    pub(crate) fn with_read_only_workspace_input(
        mut self,
        staging: DirectoryCapability,
    ) -> io::Result<Self> {
        if self.workspace_input.is_some() {
            return Err(invalid_config(
                "container configuration already has a workspace input",
            ));
        }
        validate_local_absolute_path(staging.root(), "workspace staging path")?;
        self.workspace_input = Some(staging);
        Ok(self)
    }

    /// Serialize the broker's fixed HCS v1 container document.
    pub fn to_hcs_json(&self) -> io::Result<String> {
        let writable_layer_folder = self.writable_layer_folder.as_ref().ok_or_else(|| {
            invalid_config("container configuration requires a formatted writable layer")
        })?;
        validate_local_absolute_path(writable_layer_folder, "writable layer path")?;
        let writable_layer_folder = writable_layer_folder
            .to_str()
            .ok_or_else(|| invalid_config("writable layer path is not valid Unicode"))?;
        let layers =
            self.layers
                .iter()
                .map(|layer| {
                    let path = layer.path.to_str().ok_or_else(|| {
                        invalid_config("container layer path is not valid Unicode")
                    })?;
                    Ok(HcsLayer {
                        id: layer.id.clone(),
                        path: path.to_owned(),
                    })
                })
                .collect::<io::Result<Vec<_>>>()?;
        let mapped_directories =
            self.workspace_input
                .as_ref()
                .map(|staging| {
                    let host_path = staging.root().to_str().ok_or_else(|| {
                        invalid_config("workspace staging path is not valid Unicode")
                    })?;
                    validate_local_absolute_path(staging.root(), "workspace staging path")?;
                    Ok::<_, io::Error>(vec![HcsMappedDirectory {
                        host_path: host_path.to_owned(),
                        container_path: WORKSPACE_INPUT_CONTAINER_PATH,
                        read_only: true,
                        bandwidth_maximum: 0,
                        iops_maximum: 0,
                        create_in_utility_vm: false,
                    }])
                })
                .transpose()?;
        let document = HcsV1ContainerConfig {
            system_type: "Container",
            name: &self.name,
            owner: HCS_OWNER,
            layer_folder_path: writable_layer_folder,
            layers,
            storage_sandbox_size: self.storage_sandbox_size_bytes,
            hyper_v_partition: true,
            terminate_on_last_handle_closed: true,
            mapped_directories,
        };
        serde_json::to_string(&document)
            .map_err(|_| invalid_config("container configuration could not be serialized"))
    }
}

#[derive(Serialize)]
struct HcsV1ContainerConfig<'a> {
    #[serde(rename = "SystemType")]
    system_type: &'static str,
    #[serde(rename = "Name")]
    name: &'a str,
    #[serde(rename = "Owner")]
    owner: &'static str,
    #[serde(rename = "LayerFolderPath")]
    layer_folder_path: &'a str,
    #[serde(rename = "Layers")]
    layers: Vec<HcsLayer>,
    #[serde(rename = "StorageSandboxSize")]
    storage_sandbox_size: u64,
    #[serde(rename = "HvPartition")]
    hyper_v_partition: bool,
    #[serde(rename = "TerminateOnLastHandleClosed")]
    terminate_on_last_handle_closed: bool,
    #[serde(rename = "MappedDirectories", skip_serializing_if = "Option::is_none")]
    mapped_directories: Option<Vec<HcsMappedDirectory>>,
}

#[derive(Serialize)]
struct HcsLayer {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Path")]
    path: String,
}

#[derive(Serialize)]
struct HcsMappedDirectory {
    #[serde(rename = "HostPath")]
    host_path: String,
    #[serde(rename = "ContainerPath")]
    container_path: &'static str,
    #[serde(rename = "ReadOnly")]
    read_only: bool,
    #[serde(rename = "BandwidthMaximum")]
    bandwidth_maximum: u64,
    #[serde(rename = "IOPSMaximum")]
    iops_maximum: u64,
    #[serde(rename = "CreateInUtilityVM")]
    create_in_utility_vm: bool,
}

fn validate_local_absolute_path(path: &Path, description: &str) -> io::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid_config(&format!("{description} is not valid Unicode")))?;
    if !path.is_absolute()
        || value.starts_with("\\\\")
        || value.starts_with("//")
        || value.chars().any(char::is_control)
        || value
            .split(['\\', '/'])
            .any(|component| component == "." || component == "..")
    {
        return Err(invalid_config(&format!(
            "{description} is not a local absolute path"
        )));
    }
    Ok(())
}

fn invalid_config(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;

    #[test]
    fn hcs_config_forces_hyperv_and_storage_cap_without_optional_mounts_or_network() {
        let layer = WindowsContainerLayer::new("base-layer-guid", r"C:\Haven\image\base")
            .expect("trusted local layer path is valid");
        let config = WindowsContainerSandboxConfig::new(
            "HavenSandbox-0123456789abcdef0123456789abcdef",
            1024 * 1024 * 1024,
            vec![layer],
        )
        .expect("positive storage cap and base layer are required")
        .with_test_writable_layer_folder(PathBuf::from(r"C:\Haven\runtime\attempt\layer"))
        .expect("test writable layer path is local");

        let value: Value =
            serde_json::from_str(&config.to_hcs_json().unwrap()).expect("valid HCS JSON");
        assert_eq!(value["SystemType"], "Container");
        assert_eq!(value["Owner"], HCS_OWNER);
        assert_eq!(value["HvPartition"], true);
        assert_eq!(value["TerminateOnLastHandleClosed"], true);
        assert_eq!(value["LayerFolderPath"], r"C:\Haven\runtime\attempt\layer");
        assert_eq!(value["StorageSandboxSize"], 1024 * 1024 * 1024u64);
        assert_eq!(value["Layers"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["Layers"][0],
            serde_json::json!({
                "ID": "base-layer-guid",
                "Path": r"C:\Haven\image\base"
            })
        );
        let keys: BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "HvPartition",
                "LayerFolderPath",
                "Layers",
                "Name",
                "Owner",
                "StorageSandboxSize",
                "SystemType",
                "TerminateOnLastHandleClosed",
            ])
        );
    }

    #[test]
    fn hcs_config_allows_only_one_fixed_read_only_workspace_input_mapping() {
        let temp = tempfile::tempdir().expect("staging directory is created");
        let staging = DirectoryCapability::open(temp.path()).expect("staging is pinned");
        let expected_host_path = staging.root().to_str().unwrap().to_owned();
        let config = WindowsContainerSandboxConfig::new(
            "HavenSandbox-0123456789abcdef0123456789abcdef",
            1024 * 1024 * 1024,
            vec![test_layer()],
        )
        .expect("valid base config")
        .with_test_writable_layer_folder(PathBuf::from(r"C:\Haven\runtime\attempt\layer"))
        .expect("test writable layer path is local")
        .with_read_only_workspace_input(staging)
        .expect("read-only input is accepted");

        let value: Value =
            serde_json::from_str(&config.to_hcs_json().unwrap()).expect("valid HCS JSON");
        let mappings = value["MappedDirectories"].as_array().unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0]["HostPath"], expected_host_path);
        assert_eq!(mappings[0]["ContainerPath"], WORKSPACE_INPUT_CONTAINER_PATH);
        assert_eq!(mappings[0]["ReadOnly"], true);
        assert_eq!(mappings[0]["BandwidthMaximum"], 0);
        assert_eq!(mappings[0]["IOPSMaximum"], 0);
        assert_eq!(mappings[0]["CreateInUtilityVM"], false);
        let mapping_keys: BTreeSet<_> = mappings[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            mapping_keys,
            BTreeSet::from([
                "BandwidthMaximum",
                "ContainerPath",
                "CreateInUtilityVM",
                "HostPath",
                "IOPSMaximum",
                "ReadOnly",
            ])
        );
        assert!(
            config
                .with_read_only_workspace_input(
                    DirectoryCapability::open(temp.path()).expect("second staging capability")
                )
                .is_err()
        );
    }

    #[test]
    fn hcs_config_rejects_zero_quota_and_missing_base_layers() {
        assert!(WindowsContainerSandboxConfig::new("run", 0, vec![test_layer()]).is_err());
        assert!(WindowsContainerSandboxConfig::new("run", 1, Vec::new()).is_err());
        assert!(WindowsContainerSandboxConfig::new("run\n", 1, vec![test_layer()]).is_err());
    }

    #[test]
    fn hcs_config_requires_a_formatted_attempt_layer_before_serializing() {
        let config = WindowsContainerSandboxConfig::new("run", 1, vec![test_layer()])
            .expect("base config is valid");
        assert!(config.to_hcs_json().is_err());
        assert!(
            config
                .with_test_writable_layer_folder(PathBuf::from(r"\\server\share\layer"))
                .is_err()
        );
    }

    #[test]
    fn hcs_config_rejects_unformatted_or_mismatched_scratch_layers() {
        let temp = tempfile::tempdir().expect("scratch root is created");
        let root = DirectoryCapability::open(temp.path()).expect("scratch root is pinned");
        let quota = 16 * 1024 * 1024;
        let scratch = scratch_vhd::BoundedScratchVhd::create(root, quota)
            .expect("bounded VHDX can be created without formatting privilege");
        let matching = WindowsContainerSandboxConfig::new("run", quota, vec![test_layer()])
            .expect("base config is valid");
        let mismatched = WindowsContainerSandboxConfig::new("run", quota + 512, vec![test_layer()])
            .expect("base config is valid");

        assert!(
            matching
                .clone()
                .with_bounded_writable_layer(&scratch)
                .is_err()
        );
        assert!(mismatched.with_bounded_writable_layer(&scratch).is_err());
    }

    #[test]
    fn hcs_layer_rejects_relative_parent_and_unc_paths() {
        for path in [
            PathBuf::from(r"C:\Haven\..\outside"),
            PathBuf::from(r"C:\Haven\.\image"),
            PathBuf::from(r"\\server\share\image"),
            PathBuf::from("relative\\image"),
            PathBuf::from("C:\\Haven\\invalid\npath"),
        ] {
            assert!(WindowsContainerLayer::new("base-layer-guid", path).is_err());
        }
        assert!(WindowsContainerLayer::new("bad\nid", r"C:\Haven\image\base").is_err());
    }

    fn test_layer() -> WindowsContainerLayer {
        WindowsContainerLayer::new("base-layer-guid", r"C:\Haven\image\base")
            .expect("test layer is valid")
    }
}
