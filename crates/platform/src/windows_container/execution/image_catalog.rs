//! Fixed, administrator-provisioned source for the broker's HCS image layers.
//!
//! Haven does not download or select images per request. The broker reads one
//! strict catalog beneath its protected Program Files root and pins every
//! listed layer directory for the catalog lifetime.

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
const CATALOG_VERSION: u32 = 1;
const MAX_CATALOG_BYTES: u64 = 64 * 1024;
const MAX_IMAGE_LAYERS: usize = 64;
const MAX_LAYER_DIRECTORY_NAME: usize = 64;

/// One fixed, trusted image catalog loaded from the broker installation root.
/// The layer directories stay pinned while this value is alive.
pub(crate) struct BrokerContainerImageCatalog {
    layers: Vec<WindowsContainerLayer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogDocument {
    version: u32,
    layers: Vec<CatalogLayer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogLayer {
    id: String,
    directory: String,
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

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the broker HCS provider consumes the fixed image after quota attestation"
        )
    )]
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

        Ok(Self { layers })
    }
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
            r#"{"version":1,"layers":[{"id":"child-id","directory":"child"},{"id":"base-id","directory":"base"}]}"#,
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
            r#"{"version":1,"layers":[{"id":"base","directory":"base","path":"C:\\evil"}]}"#,
            r#"{"version":2,"layers":[{"id":"base","directory":"base"}]}"#,
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
            r#"{"version":1,"layers":[{"id":"base","directory":"../base"}]}"#,
            &["base"],
        );
        assert!(
            BrokerContainerImageCatalog::load(DirectoryCapability::open(traversal.path()).unwrap())
                .is_err()
        );

        let duplicate_ids = catalog_root(
            r#"{"version":1,"layers":[{"id":"same","directory":"base"},{"id":"SAME","directory":"child"}]}"#,
            &["base", "child"],
        );
        assert!(
            BrokerContainerImageCatalog::load(
                DirectoryCapability::open(duplicate_ids.path()).unwrap()
            )
            .is_err()
        );

        let duplicate_directories = catalog_root(
            r#"{"version":1,"layers":[{"id":"base-one","directory":"base"},{"id":"base-two","directory":"BASE"}]}"#,
            &["base", "child"],
        );
        assert!(
            BrokerContainerImageCatalog::load(
                DirectoryCapability::open(duplicate_directories.path()).unwrap()
            )
            .is_err()
        );

        let root = catalog_root(
            r#"{"version":1,"layers":[{"id":"base","directory":"base"}]}"#,
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
            r#"{"version":1,"layers":[{"id":"base","directory":"missing"}]}"#,
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
}
