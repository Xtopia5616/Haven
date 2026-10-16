//! Native HCS export for a stopped, broker-owned writable layer.
//!
//! `HcsExportLayer` is synchronous and has no cancellation or output-size
//! parameter. The native call runs only in a single-process broker helper with
//! a Job Object deadline; the result is parsed by `hcs_layer_output`, which
//! accepts the known legacy `Files` transport layout.

use super::WindowsContainerSandboxConfig;
use super::hcs_export_transport::BoundedHcsExportTransport;
#[cfg(test)]
use super::hcs_layer_output::{HcsLayerOutputSummary, extract_workspace_output};
use super::hcs_runtime::HcsStoppedComputeSystem;
use crate::filesystem::DirectoryCapability;
#[cfg(test)]
use crate::filesystem::DirectoryTreeDiffLimits;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

const WRITABLE_VHDX_NAME: &str = "sandbox.vhdx";
const EXPORT_WORKER_PROTOCOL_VERSION: u16 = 2;
const MAX_EXPORT_WORKER_REQUEST_BYTES: usize = 256 * 1024;
const MAX_EXPORT_WORKER_PATH_BYTES: usize = 32 * 1024;
const MAX_EXPORT_WORKER_PARENT_LAYERS: usize = 64;

/// Calls the export API only after the compute system has stopped and released
/// the writable layer. HCS export itself cannot be interrupted; callers must
/// discard the entire attempt if export or subsequent extraction fails.
#[cfg(test)]
fn export_stopped_workspace_output(
    exporter: &dyn HcsLayerExporter,
    stopped_system: &HcsStoppedComputeSystem,
    config: &WindowsContainerSandboxConfig,
    transport: &DirectoryCapability,
    destination: &DirectoryCapability,
    limits: DirectoryTreeDiffLimits,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<HcsLayerOutputSummary> {
    check_cancelled(&mut is_cancelled)?;
    let plan = HcsLayerExportPlan::new_for_test(stopped_system, config, transport, destination)?;
    check_cancelled(&mut is_cancelled)?;
    exporter.export_stopped_writable_layer(&plan)?;
    check_cancelled(&mut is_cancelled)?;
    extract_workspace_output(transport, destination, limits, is_cancelled)
}

/// Narrow seam for the synchronous ComputeStorage API and deterministic tests.
pub(super) trait HcsLayerExporter {
    fn export_stopped_writable_layer(&self, plan: &HcsLayerExportPlan) -> io::Result<()>;
}

/// Validated inputs for exporting the writable layer that belongs to one
/// closed HCS config. Paths are derived only from private broker objects.
pub(super) struct HcsLayerExportPlan {
    _writable_layer: DirectoryCapability,
    _parent_layers: Vec<DirectoryCapability>,
    _transport_vhdx_parent: Option<DirectoryCapability>,
    writable_layer_path: PathBuf,
    export_folder_path: PathBuf,
    transport_vhdx_path: Option<PathBuf>,
    transport_capacity_bytes: Option<u64>,
    layer_data_json: String,
    options_json: String,
}

impl HcsLayerExportPlan {
    pub(super) fn encode_worker_request(&self) -> io::Result<Vec<u8>> {
        let writable_layer_path = self
            .writable_layer_path
            .to_str()
            .ok_or_else(|| invalid_data("HCS writable layer path is not valid Unicode"))?;
        let export_folder_path = self
            .export_folder_path
            .to_str()
            .ok_or_else(|| invalid_data("HCS export folder path is not valid Unicode"))?;
        let transport_vhdx_path = self
            .transport_vhdx_path
            .as_deref()
            .map(|path| {
                path.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid_data("HCS transport VHDX path is not valid Unicode"))
            })
            .transpose()?;
        let request = HcsLayerExportWorkerRequest {
            version: EXPORT_WORKER_PROTOCOL_VERSION,
            writable_layer_path: writable_layer_path.to_owned(),
            export_folder_path: export_folder_path.to_owned(),
            transport_vhdx_path,
            transport_capacity_bytes: self.transport_capacity_bytes,
            layer_data_json: self.layer_data_json.clone(),
        };
        let bytes = serde_json::to_vec(&request)
            .map_err(|_| invalid_data("HCS export worker request could not be serialized"))?;
        if bytes.len() > MAX_EXPORT_WORKER_REQUEST_BYTES {
            return Err(invalid_input(
                "HCS export worker request exceeds its size limit",
            ));
        }
        Ok(bytes)
    }

    pub(super) fn new(
        stopped_system: &HcsStoppedComputeSystem,
        config: &WindowsContainerSandboxConfig,
        transport: &BoundedHcsExportTransport,
        destination: &DirectoryCapability,
    ) -> io::Result<Self> {
        Self::new_with_transport(
            stopped_system,
            config,
            transport.directory()?,
            Some(transport.vhdx_path()?),
            Some(transport.maximum_size_bytes()?),
            destination,
        )
    }

    #[cfg(test)]
    fn new_for_test(
        stopped_system: &HcsStoppedComputeSystem,
        config: &WindowsContainerSandboxConfig,
        transport: &DirectoryCapability,
        destination: &DirectoryCapability,
    ) -> io::Result<Self> {
        Self::new_with_transport(stopped_system, config, transport, None, None, destination)
    }

    fn new_with_transport(
        stopped_system: &HcsStoppedComputeSystem,
        config: &WindowsContainerSandboxConfig,
        transport: &DirectoryCapability,
        transport_vhdx_path: Option<PathBuf>,
        transport_capacity_bytes: Option<u64>,
        destination: &DirectoryCapability,
    ) -> io::Result<Self> {
        if transport_vhdx_path.is_some() != transport_capacity_bytes.is_some() {
            return Err(invalid_input(
                "HCS transport backing path and capacity must be provided together",
            ));
        }
        let transport_vhdx_parent = if let Some(vhdx_path) = &transport_vhdx_path {
            validate_local_absolute_path(vhdx_path, "transport VHDX path")?;
            let parent_path = vhdx_path
                .parent()
                .ok_or_else(|| invalid_input("HCS transport VHDX path has no parent directory"))?;
            let parent = DirectoryCapability::open(parent_path)?;
            let file_name = vhdx_path
                .file_name()
                .ok_or_else(|| invalid_input("HCS transport VHDX path has no file name"))?;
            let vhdx = parent.open_read(Path::new(file_name))?;
            let metadata = vhdx.metadata()?;
            if !metadata.is_file() || metadata.len() == 0 {
                return Err(invalid_data("HCS transport VHDX is invalid"));
            }
            Some(parent)
        } else {
            None
        };
        if config.layers.is_empty() || config.layers.len() > MAX_EXPORT_WORKER_PARENT_LAYERS {
            return Err(invalid_input(
                "HCS parent layer count is outside the worker limit",
            ));
        }
        let writable_layer_path = config
            .writable_layer_folder
            .as_deref()
            .ok_or_else(|| invalid_input("HCS export requires a configured writable layer"))?;
        validate_local_absolute_path(writable_layer_path, "writable layer path")?;
        if !writable_layer_path.is_dir() {
            return Err(invalid_data("HCS writable layer folder is unavailable"));
        }

        let writable_layer = DirectoryCapability::open(writable_layer_path)?;
        let vhdx = writable_layer.open_read(Path::new(WRITABLE_VHDX_NAME))?;
        let vhdx_metadata = vhdx.metadata()?;
        if !vhdx_metadata.is_file() || vhdx_metadata.len() == 0 {
            return Err(invalid_data("HCS writable layer VHDX is invalid"));
        }

        if !transport.entries(1)?.is_empty() {
            return Err(invalid_input("HCS layer export destination must be empty"));
        }
        if !destination.entries(1)?.is_empty() {
            return Err(invalid_input(
                "HCS workspace output destination must be empty",
            ));
        }

        let writable_layer_path = writable_layer.root().to_path_buf();
        if normalize_path(stopped_system.writable_layer_path())
            != normalize_path(&writable_layer_path)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "HCS stopped-system proof belongs to a different writable layer",
            ));
        }
        let transport_path = transport.root().to_path_buf();
        let destination_path = destination.root().to_path_buf();
        let mut roots = vec![
            writable_layer_path.clone(),
            transport_path.clone(),
            destination_path,
        ];
        if let Some(transport_vhdx_parent) = &transport_vhdx_parent {
            roots.push(transport_vhdx_parent.root().to_path_buf());
        }
        let mut seen_layer_ids = HashSet::new();
        let mut parent_layers = Vec::with_capacity(config.layers.len());
        let mut parent_capabilities = Vec::with_capacity(config.layers.len());
        for layer in &config.layers {
            if !seen_layer_ids.insert(layer.id.to_ascii_lowercase()) {
                return Err(invalid_data("HCS parent layer IDs are not unique"));
            }
            let parent = DirectoryCapability::open(&layer.path)?;
            let parent_path = parent.root().to_path_buf();
            roots.push(parent_path.clone());
            parent_layers.push((layer.id.clone(), parent_path));
            parent_capabilities.push(parent);
        }
        ensure_disjoint_roots(&roots)?;

        let layer_data_json = serialize_layer_data(&parent_layers)?;
        let options_json = serde_json::to_string(&HcsExportOptions {
            is_writable_layer: true,
        })
        .map_err(|_| invalid_data("HCS export options could not be serialized"))?;

        Ok(Self {
            _writable_layer: writable_layer,
            _parent_layers: parent_capabilities,
            _transport_vhdx_parent: transport_vhdx_parent,
            writable_layer_path,
            export_folder_path: transport_path,
            transport_vhdx_path,
            transport_capacity_bytes,
            layer_data_json,
            options_json,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HcsLayerExportWorkerRequest {
    version: u16,
    writable_layer_path: String,
    export_folder_path: String,
    transport_vhdx_path: Option<String>,
    transport_capacity_bytes: Option<u64>,
    layer_data_json: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HcsLayerData {
    #[serde(rename = "Layers")]
    layers: Vec<HcsExportLayerDescriptor>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HcsExportLayerDescriptor {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Path")]
    path: String,
}

/// Read and validate one private worker request. This is intentionally
/// separate from the native call so protocol and path failures are testable
/// without starting HCS.
pub(super) fn decode_worker_request(reader: &mut impl Read) -> io::Result<HcsLayerExportPlan> {
    let mut length_bytes = [0_u8; 4];
    reader.read_exact(&mut length_bytes)?;
    let request_length = u32::from_le_bytes(length_bytes) as usize;
    if request_length == 0 || request_length > MAX_EXPORT_WORKER_REQUEST_BYTES {
        return Err(invalid_data(
            "HCS export worker request length is outside its limit",
        ));
    }
    let mut bytes = vec![0; request_length];
    reader.read_exact(&mut bytes)?;
    let mut trailing = [0_u8; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(invalid_data(
            "HCS export worker accepts exactly one request",
        ));
    }
    let request: HcsLayerExportWorkerRequest = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_data("HCS export worker request is malformed"))?;
    HcsLayerExportPlan::from_worker_request(request)
}

/// Entry point for the private broker executable mode. The caller verifies the
/// inherited restricted service token before accepting any request bytes.
pub(super) fn run_native_worker(reader: &mut impl Read) -> io::Result<()> {
    let plan = decode_worker_request(reader)?;
    NativeHcsLayerExporter::load()?.export_stopped_writable_layer(&plan)
}

impl HcsLayerExportPlan {
    fn from_worker_request(request: HcsLayerExportWorkerRequest) -> io::Result<Self> {
        if request.version != EXPORT_WORKER_PROTOCOL_VERSION {
            return Err(invalid_data(
                "HCS export worker protocol version is unsupported",
            ));
        }
        validate_worker_path(&request.writable_layer_path, "writable layer path")?;
        #[cfg(test)]
        validate_worker_path(&request.export_folder_path, "export folder path")?;
        #[cfg(not(test))]
        validate_hcs_export_folder_path(&request.export_folder_path)?;
        if request.layer_data_json.len() > MAX_EXPORT_WORKER_REQUEST_BYTES {
            return Err(invalid_data("HCS parent layer data exceeds its limit"));
        }

        let writable_layer = DirectoryCapability::open(Path::new(&request.writable_layer_path))?;
        let writable_vhdx = writable_layer.open_read(Path::new(WRITABLE_VHDX_NAME))?;
        let writable_vhdx_metadata = writable_vhdx.metadata()?;
        if !writable_vhdx_metadata.is_file() || writable_vhdx_metadata.len() == 0 {
            return Err(invalid_data("HCS writable layer VHDX is invalid"));
        }
        let (export_folder, transport_vhdx_parent) = match (
            request.transport_vhdx_path.as_deref(),
            request.transport_capacity_bytes,
        ) {
            (Some(vhdx_path), Some(capacity_bytes)) => {
                validate_worker_path(vhdx_path, "transport VHDX path")?;
                let (export_folder, transport_vhdx_parent) =
                    super::hcs_export_transport::open_verified_export_directory(
                        Path::new(vhdx_path),
                        Path::new(&request.export_folder_path),
                        capacity_bytes,
                    )?;
                (export_folder, Some(transport_vhdx_parent))
            }
            #[cfg(test)]
            (None, None) => (
                DirectoryCapability::open(Path::new(&request.export_folder_path))?,
                None,
            ),
            _ => {
                return Err(invalid_data(
                    "HCS export worker requires a bounded transport VHDX and capacity",
                ));
            }
        };
        if !export_folder.entries(1)?.is_empty() {
            return Err(invalid_input("HCS export folder must be empty"));
        }

        let layer_data: HcsLayerData = serde_json::from_str(&request.layer_data_json)
            .map_err(|_| invalid_data("HCS parent layer data is malformed"))?;
        if layer_data.layers.is_empty() || layer_data.layers.len() > MAX_EXPORT_WORKER_PARENT_LAYERS
        {
            return Err(invalid_data("HCS parent layer count is outside its limit"));
        }
        let mut roots = vec![
            writable_layer.root().to_path_buf(),
            export_folder.root().to_path_buf(),
        ];
        if let Some(transport_vhdx_parent) = &transport_vhdx_parent {
            roots.push(transport_vhdx_parent.root().to_path_buf());
        }
        let mut seen_layer_ids = HashSet::new();
        let mut parent_layers = Vec::with_capacity(layer_data.layers.len());
        let mut parent_capabilities = Vec::with_capacity(layer_data.layers.len());
        for layer in layer_data.layers {
            if layer.id.is_empty()
                || layer.id.len() > 256
                || layer.id.chars().any(char::is_control)
                || !seen_layer_ids.insert(layer.id.to_ascii_lowercase())
            {
                return Err(invalid_data("HCS parent layer ID is invalid or repeated"));
            }
            validate_worker_path(&layer.path, "parent layer path")?;
            let parent = DirectoryCapability::open(Path::new(&layer.path))?;
            let parent_path = parent.root().to_path_buf();
            roots.push(parent_path.clone());
            parent_layers.push((layer.id, parent_path));
            parent_capabilities.push(parent);
        }
        ensure_disjoint_roots(&roots)?;

        let writable_layer_path = writable_layer.root().to_path_buf();
        let export_folder_path = export_folder.root().to_path_buf();
        let layer_data_json = serialize_layer_data(&parent_layers)?;
        let options_json = serde_json::to_string(&HcsExportOptions {
            is_writable_layer: true,
        })
        .map_err(|_| invalid_data("HCS export options could not be serialized"))?;
        Ok(Self {
            _writable_layer: writable_layer,
            _parent_layers: parent_capabilities,
            _transport_vhdx_parent: transport_vhdx_parent,
            writable_layer_path,
            export_folder_path,
            transport_vhdx_path: request.transport_vhdx_path.map(PathBuf::from),
            transport_capacity_bytes: request.transport_capacity_bytes,
            layer_data_json,
            options_json,
        })
    }
}

fn validate_worker_path(path: &str, description: &str) -> io::Result<()> {
    if path.len() > MAX_EXPORT_WORKER_PATH_BYTES {
        return Err(invalid_data("HCS export worker path exceeds its limit"));
    }
    validate_local_absolute_path(Path::new(path), description)
}

fn validate_hcs_export_folder_path(path: &str) -> io::Result<()> {
    if path.len() > MAX_EXPORT_WORKER_PATH_BYTES {
        return Err(invalid_data("HCS export worker path exceeds its limit"));
    }
    let path = Path::new(path);
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|value| value.chars().any(char::is_control))
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
        || !super::hcs_export_transport::is_canonical_export_transport_path(path)
    {
        return Err(invalid_data(
            "HCS export folder must be the fixed child of a canonical volume GUID root",
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct HcsExportOptions {
    #[serde(rename = "IsWritableLayer")]
    is_writable_layer: bool,
}

fn serialize_layer_data(layers: &[(String, PathBuf)]) -> io::Result<String> {
    let layers = layers
        .iter()
        .map(|(id, path)| {
            let path = path
                .to_str()
                .ok_or_else(|| invalid_data("HCS parent layer path is not valid Unicode"))?;
            Ok(HcsExportLayerDescriptor {
                id: id.clone(),
                path: path.to_owned(),
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    serde_json::to_string(&HcsLayerData { layers })
        .map_err(|_| invalid_data("HCS parent layer data could not be serialized"))
}

struct NativeHcsLayerExporter {
    _module: OwnedModule,
    export_layer: unsafe extern "system" fn(*const u16, *const u16, *const u16, *const u16) -> i32,
}

impl NativeHcsLayerExporter {
    fn load() -> io::Result<Self> {
        let library_name = wide_null(OsStr::new("computestorage.dll"))?;
        let module = unsafe {
            LoadLibraryExW(
                library_name.as_ptr(),
                null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        let module = OwnedModule(module);
        let export_layer = unsafe { resolve(module.0, b"HcsExportLayer\0")? };
        Ok(Self {
            _module: module,
            export_layer,
        })
    }
}

impl HcsLayerExporter for NativeHcsLayerExporter {
    fn export_stopped_writable_layer(&self, plan: &HcsLayerExportPlan) -> io::Result<()> {
        let layer_path = wide_null(plan.writable_layer_path.as_os_str())?;
        let export_folder_path = wide_null(plan.export_folder_path.as_os_str())?;
        let layer_data = wide_null(OsStr::new(&plan.layer_data_json))?;
        let options = wide_null(OsStr::new(&plan.options_json))?;
        let result = unsafe {
            (self.export_layer)(
                layer_path.as_ptr(),
                export_folder_path.as_ptr(),
                layer_data.as_ptr(),
                options.as_ptr(),
            )
        };
        if result < 0 {
            return Err(io::Error::other(format!(
                "HCS writable layer export failed (HRESULT 0x{:08X})",
                result as u32
            )));
        }
        Ok(())
    }
}

struct OwnedModule(HMODULE);

impl Drop for OwnedModule {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.0) };
    }
}

unsafe fn resolve<T: Copy>(module: HMODULE, name: &'static [u8]) -> io::Result<T> {
    let symbol = unsafe { GetProcAddress(module, name.as_ptr()) }.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HCS writable layer export API is unavailable",
        )
    })?;
    if std::mem::size_of::<T>() != std::mem::size_of_val(&symbol) {
        return Err(invalid_data(
            "HCS export function pointer has an unexpected representation",
        ));
    }
    Ok(unsafe { std::mem::transmute_copy(&symbol) })
}

fn wide_null(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(invalid_input("HCS export input contains a null character"));
    }
    wide.push(0);
    Ok(wide)
}

fn validate_local_absolute_path(path: &Path, description: &str) -> io::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid_input("HCS export path is not valid Unicode"))?;
    if !path.is_absolute()
        || value.starts_with("\\\\")
        || value.starts_with("//")
        || value.chars().any(char::is_control)
        || value
            .split(['\\', '/'])
            .any(|component| component == "." || component == "..")
    {
        return Err(invalid_input(&format!(
            "{description} is not a local absolute path"
        )));
    }
    Ok(())
}

fn ensure_disjoint_roots(roots: &[PathBuf]) -> io::Result<()> {
    for (index, left) in roots.iter().enumerate() {
        let left = normalize_path(left);
        for right in &roots[index + 1..] {
            let right = normalize_path(right);
            if left == right || left.starts_with(&right) || right.starts_with(&left) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "HCS layer, export, output, and parent roots must be disjoint",
                ));
            }
        }
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase())
}

#[cfg(test)]
fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> io::Result<()> {
    if is_cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "HCS writable layer export was cancelled",
        ));
    }
    Ok(())
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::super::WindowsContainerLayer;
    use super::*;
    use crate::filesystem::DirectoryTreeCopyLimits;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;

    struct MockExporter {
        calls: Mutex<Vec<(String, String, String, String)>>,
        write_fixture: bool,
    }

    impl MockExporter {
        fn new(write_fixture: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                write_fixture,
            }
        }
    }

    impl HcsLayerExporter for MockExporter {
        fn export_stopped_writable_layer(&self, plan: &HcsLayerExportPlan) -> io::Result<()> {
            self.calls.lock().unwrap().push((
                plan.writable_layer_path.to_string_lossy().into_owned(),
                plan.export_folder_path.to_string_lossy().into_owned(),
                plan.layer_data_json.clone(),
                plan.options_json.clone(),
            ));
            if self.write_fixture {
                let root = DirectoryCapability::open(&plan.export_folder_path)?;
                write_backup_file(
                    &root,
                    Path::new(r"Files\Haven\workspace-output\answer.txt"),
                    0x20,
                    b"bounded result",
                )?;
            }
            Ok(())
        }
    }

    struct Fixture {
        _base: tempfile::TempDir,
        _writable: tempfile::TempDir,
        transport: tempfile::TempDir,
        destination: tempfile::TempDir,
        config: WindowsContainerSandboxConfig,
    }

    impl Fixture {
        fn new() -> Self {
            let base = tempfile::tempdir().unwrap();
            let writable = tempfile::tempdir().unwrap();
            let transport = tempfile::tempdir().unwrap();
            let destination = tempfile::tempdir().unwrap();
            fs::write(writable.path().join(WRITABLE_VHDX_NAME), b"test vhdx").unwrap();
            let layer =
                WindowsContainerLayer::new("base-layer", base.path()).expect("base layer is valid");
            let config = WindowsContainerSandboxConfig::new(
                "HavenSandbox-0123456789abcdef0123456789abcdef",
                4 * 1024 * 1024,
                vec![layer],
            )
            .unwrap()
            .with_test_writable_layer_folder(writable.path().to_path_buf())
            .unwrap();
            Self {
                _base: base,
                _writable: writable,
                transport,
                destination,
                config,
            }
        }

        fn capabilities(&self) -> (DirectoryCapability, DirectoryCapability) {
            (
                DirectoryCapability::open(self.transport.path()).unwrap(),
                DirectoryCapability::open(self.destination.path()).unwrap(),
            )
        }

        fn stopped_system(&self) -> HcsStoppedComputeSystem {
            HcsStoppedComputeSystem::test_permit(self._writable.path().to_path_buf())
        }
    }

    fn limits() -> DirectoryTreeDiffLimits {
        let scan = DirectoryTreeCopyLimits::new(128, 16, 1024, 4096).unwrap();
        DirectoryTreeDiffLimits::new(scan, 128, 1024, 4096).unwrap()
    }

    fn worker_frame(bytes: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(4 + bytes.len());
        frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        frame.extend_from_slice(bytes);
        frame
    }

    #[test]
    fn worker_request_round_trips_only_the_validated_export_plan() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let plan = HcsLayerExportPlan::new_for_test(
            &fixture.stopped_system(),
            &fixture.config,
            &transport,
            &destination,
        )
        .unwrap();
        let request = plan.encode_worker_request().unwrap();
        let mut frame = std::io::Cursor::new(worker_frame(&request));
        let decoded = decode_worker_request(&mut frame).unwrap();

        assert_eq!(decoded.writable_layer_path, plan.writable_layer_path);
        assert_eq!(decoded.export_folder_path, plan.export_folder_path);
        assert_eq!(decoded.layer_data_json, plan.layer_data_json);
        assert_eq!(decoded.options_json, plan.options_json);
    }

    #[test]
    fn worker_request_rejects_trailing_data_and_unsupported_version() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let plan = HcsLayerExportPlan::new_for_test(
            &fixture.stopped_system(),
            &fixture.config,
            &transport,
            &destination,
        )
        .unwrap();
        let mut frame = worker_frame(&plan.encode_worker_request().unwrap());
        frame.push(0);
        assert_eq!(
            decode_worker_request(&mut std::io::Cursor::new(frame))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let mut request: HcsLayerExportWorkerRequest =
            serde_json::from_slice(&plan.encode_worker_request().unwrap()).unwrap();
        request.version += 1;
        let bytes = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            decode_worker_request(&mut std::io::Cursor::new(worker_frame(&bytes)))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn worker_request_rejects_unpaired_transport_identity() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let plan = HcsLayerExportPlan::new_for_test(
            &fixture.stopped_system(),
            &fixture.config,
            &transport,
            &destination,
        )
        .unwrap();
        let mut request: HcsLayerExportWorkerRequest =
            serde_json::from_slice(&plan.encode_worker_request().unwrap()).unwrap();
        request.transport_capacity_bytes = Some(3 * 1024 * 1024);
        let bytes = serde_json::to_vec(&request).unwrap();

        assert_eq!(
            decode_worker_request(&mut std::io::Cursor::new(worker_frame(&bytes)))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn production_export_folder_validation_accepts_only_a_volume_child() {
        let volume_root = r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\";
        assert!(validate_hcs_export_folder_path(&format!("{volume_root}export")).is_ok());
        assert!(validate_hcs_export_folder_path(&format!("{volume_root}export\\nested")).is_err());
        assert!(validate_hcs_export_folder_path(r"C:\temp\export").is_err());
    }

    #[test]
    fn worker_request_rejects_oversize_and_overlapping_layer_roots() {
        let oversized_length = u32::try_from(MAX_EXPORT_WORKER_REQUEST_BYTES + 1)
            .unwrap()
            .to_le_bytes();
        assert_eq!(
            decode_worker_request(&mut std::io::Cursor::new(oversized_length))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let plan = HcsLayerExportPlan::new_for_test(
            &fixture.stopped_system(),
            &fixture.config,
            &transport,
            &destination,
        )
        .unwrap();
        let mut request: HcsLayerExportWorkerRequest =
            serde_json::from_slice(&plan.encode_worker_request().unwrap()).unwrap();
        let mut layer_data: HcsLayerData = serde_json::from_str(&request.layer_data_json).unwrap();
        layer_data.layers[0].path = plan.writable_layer_path.to_string_lossy().into_owned();
        request.layer_data_json = serde_json::to_string(&layer_data).unwrap();
        let bytes = serde_json::to_vec(&request).unwrap();

        assert_eq!(
            decode_worker_request(&mut std::io::Cursor::new(worker_frame(&bytes)))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    fn write_backup_file(
        root: &DirectoryCapability,
        relative: &Path,
        attributes: u32,
        content: &[u8],
    ) -> io::Result<()> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&attributes.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(content.len() as i64).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(content);
        if let Some(parent) = relative.parent() {
            root.create_dir_all(parent)?;
        }
        let mut file = root.create_new_file(relative)?;
        std::io::Write::write_all(&mut file, &bytes)
    }

    #[test]
    fn exports_stopped_layer_then_extracts_only_the_workspace_subtree() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let stopped_system = fixture.stopped_system();
        let exporter = MockExporter::new(true);
        let summary = export_stopped_workspace_output(
            &exporter,
            &stopped_system,
            &fixture.config,
            &transport,
            &destination,
            limits(),
            || false,
        )
        .expect("mock HCS export is decoded");

        assert_eq!(summary.file_count, 1);
        assert_eq!(summary.data_bytes, 14);
        assert_eq!(
            fs::read(fixture.destination.path().join("answer.txt")).unwrap(),
            b"bounded result"
        );
        let calls = exporter.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, fixture._writable.path().to_str().unwrap());
        let layer_data: serde_json::Value = serde_json::from_str(&calls[0].2).unwrap();
        assert_eq!(layer_data["Layers"][0]["Id"], "base-layer");
        assert_eq!(
            layer_data["Layers"][0]["Path"],
            fixture._base.path().to_str().unwrap()
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&calls[0].3).unwrap(),
            serde_json::json!({ "IsWritableLayer": true })
        );
    }

    #[test]
    fn cancellation_before_native_export_does_not_call_hcs() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let stopped_system = fixture.stopped_system();
        let exporter = MockExporter::new(false);
        let error = export_stopped_workspace_output(
            &exporter,
            &stopped_system,
            &fixture.config,
            &transport,
            &destination,
            limits(),
            || true,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(exporter.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn cancellation_after_native_export_requires_discarding_the_attempt() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let stopped_system = fixture.stopped_system();
        let exporter = MockExporter::new(true);
        let mut calls = 0;
        let error = export_stopped_workspace_output(
            &exporter,
            &stopped_system,
            &fixture.config,
            &transport,
            &destination,
            limits(),
            || {
                calls += 1;
                calls >= 3
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(exporter.calls.lock().unwrap().len(), 1);
        assert!(
            fixture
                .destination
                .path()
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
        assert!(fixture.transport.path().join("Files").exists());
    }

    #[test]
    fn rejects_nonempty_export_and_output_directories() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let stopped_system = fixture.stopped_system();
        fs::write(fixture.transport.path().join("existing"), b"x").unwrap();
        let exporter = MockExporter::new(false);
        assert_eq!(
            export_stopped_workspace_output(
                &exporter,
                &stopped_system,
                &fixture.config,
                &transport,
                &destination,
                limits(),
                || false,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(exporter.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn stopped_system_proof_is_bound_to_the_exported_writable_layer() {
        let fixture = Fixture::new();
        let (transport, destination) = fixture.capabilities();
        let stopped_system =
            HcsStoppedComputeSystem::test_permit(fixture._base.path().to_path_buf());
        let exporter = MockExporter::new(false);

        assert_eq!(
            export_stopped_workspace_output(
                &exporter,
                &stopped_system,
                &fixture.config,
                &transport,
                &destination,
                limits(),
                || false,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(exporter.calls.lock().unwrap().is_empty());
    }
}
