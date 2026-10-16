//! Fixed HCS v1 configuration for Haven's optional Windows container backend.
//!
//! This module builds a fixed configuration document and exposes a typed
//! execution port. It does not create a container, install services, or
//! authorize a tool call. Callers must supply image layers from the broker's
//! trusted local image catalog. The HCS schema is intentionally closed: no
//! host mounts, network endpoints, credentials, devices, or caller-selected
//! isolation mode can be serialized.

pub mod execution;
#[cfg(not(test))]
#[expect(
    dead_code,
    reason = "HCS scratch quota runtime gate has not passed, so no production provider may consume this prototype yet"
)]
pub(crate) mod scratch_vhd;
#[cfg(test)]
pub(crate) mod scratch_vhd;

use serde::Serialize;
use std::io;
use std::path::{Path, PathBuf};

const HCS_OWNER: &str = "HavenContainerBroker";

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
/// bytes. A successful serialization does not prove that a target host
/// enforces that size; runtime over-quota Windows verification remains
/// mandatory before this backend is connected to production tool execution.
#[derive(Debug, Clone)]
pub struct WindowsContainerSandboxConfig {
    name: String,
    storage_sandbox_size_bytes: u64,
    layers: Vec<WindowsContainerLayer>,
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
        })
    }

    /// Serialize the broker's fixed HCS v1 container document.
    pub fn to_hcs_json(&self) -> io::Result<String> {
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
        let document = HcsV1ContainerConfig {
            system_type: "Container",
            name: &self.name,
            owner: HCS_OWNER,
            layers,
            storage_sandbox_size: self.storage_sandbox_size_bytes,
            hyper_v_partition: true,
            terminate_on_last_handle_closed: true,
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
    #[serde(rename = "Layers")]
    layers: Vec<HcsLayer>,
    #[serde(rename = "StorageSandboxSize")]
    storage_sandbox_size: u64,
    #[serde(rename = "HvPartition")]
    hyper_v_partition: bool,
    #[serde(rename = "TerminateOnLastHandleClosed")]
    terminate_on_last_handle_closed: bool,
}

#[derive(Serialize)]
struct HcsLayer {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Path")]
    path: String,
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
    fn hcs_config_forces_hyperv_and_storage_cap_without_host_mounts_or_network() {
        let layer = WindowsContainerLayer::new("base-layer-guid", r"C:\Haven\image\base")
            .expect("trusted local layer path is valid");
        let config = WindowsContainerSandboxConfig::new(
            "HavenSandbox-0123456789abcdef0123456789abcdef",
            1024 * 1024 * 1024,
            vec![layer],
        )
        .expect("positive storage cap and base layer are required");

        let value: Value =
            serde_json::from_str(&config.to_hcs_json().unwrap()).expect("valid HCS JSON");
        assert_eq!(value["SystemType"], "Container");
        assert_eq!(value["Owner"], HCS_OWNER);
        assert_eq!(value["HvPartition"], true);
        assert_eq!(value["TerminateOnLastHandleClosed"], true);
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
    fn hcs_config_rejects_zero_quota_and_missing_base_layers() {
        assert!(WindowsContainerSandboxConfig::new("run", 0, vec![test_layer()]).is_err());
        assert!(WindowsContainerSandboxConfig::new("run", 1, Vec::new()).is_err());
        assert!(WindowsContainerSandboxConfig::new("run\n", 1, vec![test_layer()]).is_err());
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
