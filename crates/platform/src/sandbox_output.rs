//! Backend-neutral owner for one completed sandbox output review.
//!
//! Provider implementations create this only after the execution tree has
//! exited and the input baseline/output manifest have been captured. The
//! owner keeps the private output alive until the Tools review owner drops or
//! explicitly cleans it up.

use crate::filesystem::{
    DirectoryCapability, DirectoryTreeChange, DirectoryTreeFileCopySummary,
    DirectoryTreeOutputManifest,
};
use std::io;
use std::path::Path;

/// Platform-owned lifetime and promotion operations for a reviewed output
/// workspace. The concrete AppContainer/HCS workspace never crosses into
/// Tools.
pub(crate) trait SandboxOutputWorkspaceOwner: Send {
    fn execution_workspace_generation(&self) -> u64;

    fn policy_revision(&self) -> u64;

    fn initial_snapshot_byte_count(&self) -> u64;

    fn promote_created_file(
        &self,
        change: &DirectoryTreeChange,
        destination: &DirectoryCapability,
        destination_relative: &Path,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> io::Result<DirectoryTreeFileCopySummary>;

    fn cleanup(self: Box<Self>) -> io::Result<()>;
}

/// A completed sandbox run's immutable bounded manifest and private output
/// owner. File contents and absolute paths stay behind this platform type.
pub struct SandboxOutputReview {
    workspace: Box<dyn SandboxOutputWorkspaceOwner>,
    manifest: DirectoryTreeOutputManifest,
}

impl SandboxOutputReview {
    pub(crate) fn new(
        workspace: Box<dyn SandboxOutputWorkspaceOwner>,
        manifest: DirectoryTreeOutputManifest,
    ) -> Self {
        Self {
            workspace,
            manifest,
        }
    }

    pub fn manifest(&self) -> &DirectoryTreeOutputManifest {
        &self.manifest
    }

    /// Conservative upper bound for the private content retained by this
    /// review. The initial copy and baseline count twice; changed output bytes
    /// are then added without subtracting modified/deleted input bytes.
    pub fn retained_storage_upper_bound_bytes(&self) -> u64 {
        let changed_file_bytes = self
            .manifest
            .changes
            .iter()
            .filter_map(|change| change.after_bytes)
            .fold(0_u64, u64::saturating_add);
        self.workspace
            .initial_snapshot_byte_count()
            .saturating_mul(2)
            .saturating_add(changed_file_bytes)
    }

    /// Promote one manifest-created regular output file to a missing
    /// destination below an already-authorized directory capability. This
    /// method does not authorize the destination; Tools must hold its live
    /// policy permit and supply the expected workspace/policy stamps.
    pub fn promote_output_file_to(
        &self,
        change: &DirectoryTreeChange,
        expected_execution_workspace_generation: u64,
        expected_policy_revision: u64,
        destination: &DirectoryCapability,
        destination_relative: &Path,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeFileCopySummary> {
        if self.workspace.execution_workspace_generation()
            != expected_execution_workspace_generation
            || self.workspace.policy_revision() != expected_policy_revision
        {
            return Err(permission_denied(
                "sandbox output is bound to a stale execution workspace or policy revision",
            ));
        }
        if !self.manifest.changes.iter().any(|entry| entry == change) {
            return Err(permission_denied(
                "sandbox output change is not part of this reviewed manifest",
            ));
        }
        self.workspace.promote_created_file(
            change,
            destination,
            destination_relative,
            &mut is_cancelled,
        )
    }

    /// Explicitly discard this review's private output and baseline.
    pub fn cleanup(self) -> io::Result<()> {
        self.workspace.cleanup()
    }
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
