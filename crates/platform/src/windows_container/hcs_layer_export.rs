//! Native HCS export for a stopped, broker-owned writable layer.
//!
//! `HcsExportLayer` is synchronous and has no cancellation or output-size
//! parameter. This module therefore keeps the call private and leaves it
//! unwired from the broker request loop until an isolated worker owns its
//! deadline and export staging lifecycle. The result is still parsed only by
//! `hcs_layer_output`, which accepts the known legacy `Files` transport layout.

use super::WindowsContainerSandboxConfig;
use super::hcs_layer_output::{HcsLayerOutputSummary, extract_workspace_output};
use super::hcs_runtime::HcsStoppedComputeSystem;
use crate::filesystem::{DirectoryCapability, DirectoryTreeDiffLimits};
use serde::Serialize;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

const WRITABLE_VHDX_NAME: &str = "sandbox.vhdx";

/// Calls the export API only after the compute system has stopped and released
/// the writable layer. HCS export itself cannot be interrupted; callers must
/// discard the entire attempt if export or subsequent extraction fails.
pub(super) fn export_stopped_workspace_output(
    exporter: &dyn HcsLayerExporter,
    stopped_system: &HcsStoppedComputeSystem,
    config: &WindowsContainerSandboxConfig,
    transport: &DirectoryCapability,
    destination: &DirectoryCapability,
    limits: DirectoryTreeDiffLimits,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<HcsLayerOutputSummary> {
    check_cancelled(&mut is_cancelled)?;
    let plan = HcsLayerExportPlan::new(stopped_system, config, transport, destination)?;
    check_cancelled(&mut is_cancelled)?;
    exporter.export_stopped_writable_layer(&plan)?;
    check_cancelled(&mut is_cancelled)?;
    extract_workspace_output(transport, destination, limits, is_cancelled)
}

#[expect(
    dead_code,
    reason = "the broker worker will call native export after isolating its synchronous lifetime"
)]
pub(super) fn export_stopped_workspace_output_native(
    stopped_system: &HcsStoppedComputeSystem,
    config: &WindowsContainerSandboxConfig,
    transport: &DirectoryCapability,
    destination: &DirectoryCapability,
    limits: DirectoryTreeDiffLimits,
    is_cancelled: impl FnMut() -> bool,
) -> io::Result<HcsLayerOutputSummary> {
    let exporter = NativeHcsLayerExporter::load()?;
    export_stopped_workspace_output(
        &exporter,
        stopped_system,
        config,
        transport,
        destination,
        limits,
        is_cancelled,
    )
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
    writable_layer_path: PathBuf,
    export_folder_path: PathBuf,
    layer_data_json: String,
    options_json: String,
}

impl HcsLayerExportPlan {
    fn new(
        stopped_system: &HcsStoppedComputeSystem,
        config: &WindowsContainerSandboxConfig,
        transport: &DirectoryCapability,
        destination: &DirectoryCapability,
    ) -> io::Result<Self> {
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
            writable_layer_path,
            export_folder_path: transport_path,
            layer_data_json,
            options_json,
        })
    }
}

#[derive(Serialize)]
struct HcsLayerData {
    #[serde(rename = "Layers")]
    layers: Vec<HcsExportLayerDescriptor>,
}

#[derive(Serialize)]
struct HcsExportLayerDescriptor {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Path")]
    path: String,
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
