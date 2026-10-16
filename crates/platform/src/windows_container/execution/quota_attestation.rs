//! Runtime proof that the broker's fixed HCS scratch quota reaches guest writes.
//!
//! A successful proof is private, process-local state. It owns the exact native
//! HCS API module and is bound to the pinned image catalog and one quota value.
//! The broker never accepts quota claims from the named-pipe client.

use super::broker_workspace::{BrokerWorkspaceAttempt, BrokerWorkspaceScratchRoot};
use super::image_catalog::BrokerContainerImageCatalog;
use super::named_pipe::BrokerPipeCancellation;
use crate::windows_container::execution::WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
use crate::windows_container::hcs_guest_execution::{
    HcsGuestExecutionFailure, execute_quota_probe, native_api,
};
use crate::windows_container::hcs_runtime::{HcsApi, NativeHcsApi};
use crate::windows_container::scratch_vhd::BoundedScratchVhd;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

const QUOTA_PROBE_TIMEOUT: Duration = Duration::from_secs(4 * 60);
const QUOTA_PROBE_EXTRA_WRITE_BYTES: u64 = 1024 * 1024;
const MIN_QUOTA_PROBE_WRITE_BYTES: u64 = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES / 2;
const MIN_HOST_FREE_SPACE_BYTES: u64 = 1024 * 1024 * 1024;

/// Process-local evidence that the fixed quota is enforced by the selected
/// HCS runtime. The native API module remains loaded for subsequent runs.
pub(super) struct QuotaRuntimeAttestation {
    storage_quota_bytes: u64,
    skill_python_runtime_attested: bool,
    mcp_node_runtime_attested: bool,
    api: NativeHcsApi,
    image_catalog: BrokerContainerImageCatalog,
}

impl QuotaRuntimeAttestation {
    pub(super) fn probe(
        workspace_root: &BrokerWorkspaceScratchRoot,
        image_catalog: &BrokerContainerImageCatalog,
        cancellation: &BrokerPipeCancellation,
    ) -> io::Result<Self> {
        let api = native_api()?;
        let storage_quota_bytes = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let skill_python_runtime = image_catalog.skill_python_runtime().cloned();
        let mcp_node_runtime = image_catalog.mcp_node_runtime().cloned();
        let mut attempt = workspace_root.create_attempt()?;
        let attempt_root = attempt.attempt_root()?.clone();
        let mut scratch = BoundedScratchVhd::create(attempt_root, storage_quota_bytes)?;

        let config = match prepare_probe(&mut scratch, image_catalog, storage_quota_bytes) {
            Ok(config) => config,
            Err(error) => {
                cleanup_attempt(&mut attempt, &mut scratch, false)?;
                return Err(error);
            }
        };
        let physical_size_before = match scratch.physical_size_bytes() {
            Ok(size) => size,
            Err(error) => {
                cleanup_attempt(&mut attempt, &mut scratch, false)?;
                return Err(error);
            }
        };
        let host_space_before = match available_host_space(&scratch.path()) {
            Ok(space) => space,
            Err(error) => {
                cleanup_attempt(&mut attempt, &mut scratch, false)?;
                return Err(error);
            }
        };
        if validate_host_space_before(storage_quota_bytes, host_space_before).is_err() {
            cleanup_attempt(&mut attempt, &mut scratch, false)?;
            return Err(unattested(
                "host volume does not have the required probe reserve",
            ));
        }

        if attempt.mark_hcs_operation_active().is_err() {
            quarantine_attempt(&mut attempt, &mut scratch);
            return Err(unattested("quota probe attempt could not be marked active"));
        }

        let outcome = match execute_quota_probe(
            &api,
            &config,
            storage_quota_bytes,
            skill_python_runtime.as_ref(),
            mcp_node_runtime.as_ref(),
            QUOTA_PROBE_TIMEOUT,
            || cancellation.is_cancelled(),
        ) {
            Ok(outcome) => outcome,
            Err(failure) => {
                if failure.cleanup_safe {
                    cleanup_attempt(&mut attempt, &mut scratch, true)?;
                } else {
                    quarantine_attempt(&mut attempt, &mut scratch);
                }
                return Err(quota_probe_failure(failure));
            }
        };

        let evidence = (|| {
            if outcome.stopped_system.writable_layer_path() != scratch.layer_path()? {
                return Err(unattested(
                    "quota probe stop proof belongs to a different writable layer",
                ));
            }
            scratch.verify_maximum_size()?;
            let physical_size_after = scratch.physical_size_bytes()?;
            let host_space_after = available_host_space(&scratch.path())?;
            validate_probe_evidence(
                storage_quota_bytes,
                ProbeProcessEvidence {
                    exit_code: outcome.exit_code,
                    stdout: &outcome.stdout,
                    stderr: &outcome.stderr,
                    stdout_truncated: outcome.stdout_truncated,
                    stderr_truncated: outcome.stderr_truncated,
                },
                ScratchGrowthEvidence {
                    physical_size_before,
                    physical_size_after,
                },
                host_space_after,
                skill_python_runtime.is_some(),
                mcp_node_runtime.is_some(),
            )
        })();
        drop(outcome);
        let evidence = match evidence {
            Ok(evidence) => evidence,
            Err(error) => {
                cleanup_attempt(&mut attempt, &mut scratch, true)?;
                return Err(error);
            }
        };
        cleanup_attempt(&mut attempt, &mut scratch, true)?;

        Ok(Self {
            storage_quota_bytes,
            skill_python_runtime_attested: evidence.skill_python_runtime_attested,
            mcp_node_runtime_attested: evidence.mcp_node_runtime_attested,
            api,
            image_catalog: image_catalog.clone(),
        })
    }

    pub(super) fn accepts_quota(&self, storage_quota_bytes: u64) -> bool {
        self.storage_quota_bytes == storage_quota_bytes
    }

    pub(super) fn api(&self) -> &dyn HcsApi {
        &self.api
    }

    pub(super) fn image_catalog(&self) -> &BrokerContainerImageCatalog {
        &self.image_catalog
    }

    pub(super) fn skill_python_runtime_attested(&self) -> bool {
        self.skill_python_runtime_attested
    }

    pub(super) fn mcp_node_runtime_attested(&self) -> bool {
        self.mcp_node_runtime_attested
    }
}

fn prepare_probe(
    scratch: &mut BoundedScratchVhd,
    image_catalog: &BrokerContainerImageCatalog,
    storage_quota_bytes: u64,
) -> io::Result<crate::windows_container::WindowsContainerSandboxConfig> {
    scratch.format_for_container()?;
    let config = image_catalog.sandbox_config(probe_system_name(), storage_quota_bytes)?;
    // No mapped host directory or user workspace is attached to the probe.
    config.with_bounded_writable_layer(scratch)
}

fn probe_system_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("HavenQuotaProbe-{}-{timestamp}", std::process::id())
}

fn cleanup_attempt(
    attempt: &mut BrokerWorkspaceAttempt,
    scratch: &mut BoundedScratchVhd,
    hcs_operation_marked: bool,
) -> io::Result<()> {
    if hcs_operation_marked && attempt.mark_hcs_operation_quiescent().is_err() {
        quarantine_attempt(attempt, scratch);
        return Err(unattested("quota probe could not prove attempt quiescence"));
    }
    if scratch.cleanup().is_err() {
        quarantine_attempt(attempt, scratch);
        return Err(unattested("quota probe scratch cleanup failed"));
    }
    if attempt.cleanup().is_err() {
        let _ = attempt.preserve_for_recovery();
        return Err(unattested("quota probe attempt cleanup failed"));
    }
    Ok(())
}

fn quarantine_attempt(attempt: &mut BrokerWorkspaceAttempt, scratch: &mut BoundedScratchVhd) {
    scratch.preserve_for_recovery();
    let _ = attempt.preserve_for_recovery();
}

fn quota_probe_failure(failure: HcsGuestExecutionFailure) -> io::Error {
    let reason = match (failure.kind, failure.cleanup_safe) {
        (_, false) => "HCS quota probe stop could not be confirmed",
        (super::super::hcs_guest_execution::HcsGuestExecutionFailureKind::TimedOut, true) => {
            "HCS quota probe timed out"
        }
        (super::super::hcs_guest_execution::HcsGuestExecutionFailureKind::Cancelled, true) => {
            "HCS quota probe was interrupted"
        }
        _ => "HCS quota probe failed",
    };
    unattested(reason)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostSpace {
    available_to_service: u64,
    total_free: u64,
}

struct ProbeProcessEvidence<'a> {
    exit_code: u32,
    stdout: &'a [u8],
    stderr: &'a [u8],
    stdout_truncated: bool,
    stderr_truncated: bool,
}

struct ScratchGrowthEvidence {
    physical_size_before: u64,
    physical_size_after: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuotaProbeEvidence {
    written_bytes: u64,
    skill_python_runtime_attested: bool,
    mcp_node_runtime_attested: bool,
}

fn available_host_space(path: &Path) -> io::Result<HostSpace> {
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "quota probe path has no parent",
        )
    })?;
    let mut wide_path = directory.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide_path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "quota probe directory contains a null character",
        ));
    }
    wide_path.push(0);
    let mut available_to_service = 0;
    let mut total_bytes = 0;
    let mut total_free = 0;
    // SAFETY: the path is terminated UTF-16 and all output pointers remain
    // valid for this synchronous Windows API call.
    if unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut available_to_service,
            &mut total_bytes,
            &mut total_free,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _ = total_bytes;
    Ok(HostSpace {
        available_to_service,
        total_free,
    })
}

fn validate_host_space_before(quota: u64, space: HostSpace) -> io::Result<()> {
    let required = quota
        .checked_add(MIN_HOST_FREE_SPACE_BYTES)
        .ok_or_else(|| unattested("quota probe host-space requirement overflowed"))?;
    if space.available_to_service < required || space.total_free < required {
        return Err(unattested("host volume lacks quota probe headroom"));
    }
    Ok(())
}

fn validate_probe_evidence(
    quota: u64,
    process: ProbeProcessEvidence<'_>,
    scratch: ScratchGrowthEvidence,
    host_space_after: HostSpace,
    skill_profile_configured: bool,
    mcp_node_profile_configured: bool,
) -> io::Result<QuotaProbeEvidence> {
    if process.exit_code != 0
        || process.stdout_truncated
        || process.stderr_truncated
        || !process.stderr.is_empty()
    {
        return Err(unattested("quota probe process result is incomplete"));
    }
    let evidence = parse_probe_output(process.stdout)?;
    if evidence.skill_python_runtime_attested && !skill_profile_configured {
        return Err(unattested(
            "quota probe reported an unconfigured Skill runtime",
        ));
    }
    if evidence.mcp_node_runtime_attested && !mcp_node_profile_configured {
        return Err(unattested(
            "quota probe reported an unconfigured MCP Node runtime",
        ));
    }
    let maximum_write = quota
        .checked_add(QUOTA_PROBE_EXTRA_WRITE_BYTES)
        .ok_or_else(|| unattested("quota probe write bound overflowed"))?;
    if evidence.written_bytes < MIN_QUOTA_PROBE_WRITE_BYTES
        || evidence.written_bytes > maximum_write
    {
        return Err(unattested("quota probe write count is outside its bound"));
    }
    let physical_growth = scratch
        .physical_size_after
        .checked_sub(scratch.physical_size_before)
        .ok_or_else(|| unattested("scratch VHDX became smaller during the quota probe"))?;
    if physical_growth < MIN_QUOTA_PROBE_WRITE_BYTES {
        return Err(unattested(
            "quota probe did not substantially grow its bounded scratch VHDX",
        ));
    }
    if host_space_after.available_to_service < MIN_HOST_FREE_SPACE_BYTES
        || host_space_after.total_free < MIN_HOST_FREE_SPACE_BYTES
    {
        return Err(unattested(
            "host volume reserve was exhausted during the quota probe",
        ));
    }
    Ok(evidence)
}

fn parse_probe_output(stdout: &[u8]) -> io::Result<QuotaProbeEvidence> {
    let output =
        std::str::from_utf8(stdout).map_err(|_| unattested("quota probe output is not UTF-8"))?;
    let line = output
        .strip_suffix("\r\n")
        .or_else(|| output.strip_suffix('\n'))
        .unwrap_or(output);
    let values = line
        .strip_prefix("HAVEN_STORAGE_QUOTA_ATTESTED_V3:")
        .ok_or_else(|| unattested("quota probe did not return its exact success marker"))?;
    let mut parts = values.split(':');
    let (written, skill_runtime, mcp_node_runtime) =
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(written), Some(skill_runtime), Some(mcp_node_runtime), None)
                if !written.is_empty()
                    && written.bytes().all(|byte| byte.is_ascii_digit())
                    && matches!(skill_runtime, "0" | "1")
                    && matches!(mcp_node_runtime, "0" | "1") =>
            {
                (written, skill_runtime, mcp_node_runtime)
            }
            _ => return Err(unattested("quota probe returned malformed evidence")),
        };
    Ok(QuotaProbeEvidence {
        written_bytes: written
            .parse::<u64>()
            .map_err(|_| unattested("quota probe returned an invalid write count"))?,
        skill_python_runtime_attested: skill_runtime == "1",
        mcp_node_runtime_attested: mcp_node_runtime == "1",
    })
}

fn unattested(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_space(available: u64) -> HostSpace {
        HostSpace {
            available_to_service: available,
            total_free: available,
        }
    }

    fn valid_evidence(
        written: u64,
        physical_growth: u64,
        free_after: u64,
    ) -> io::Result<QuotaProbeEvidence> {
        let quota = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let stdout = format!("HAVEN_STORAGE_QUOTA_ATTESTED_V3:{written}:0:0\r\n");
        validate_probe_evidence(
            quota,
            ProbeProcessEvidence {
                exit_code: 0,
                stdout: stdout.as_bytes(),
                stderr: &[],
                stdout_truncated: false,
                stderr_truncated: false,
            },
            ScratchGrowthEvidence {
                physical_size_before: 1024 * 1024,
                physical_size_after: 1024 * 1024 + physical_growth,
            },
            host_space(free_after),
            false,
            false,
        )
    }

    #[test]
    fn quota_probe_requires_guest_full_marker_and_growth_of_the_bounded_layer() {
        let quota = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        assert_eq!(
            valid_evidence(quota / 2, quota / 2, MIN_HOST_FREE_SPACE_BYTES).unwrap(),
            QuotaProbeEvidence {
                written_bytes: quota / 2,
                skill_python_runtime_attested: false,
                mcp_node_runtime_attested: false,
            }
        );
        assert!(valid_evidence(quota / 2 - 1, quota, MIN_HOST_FREE_SPACE_BYTES).is_err());
        assert!(valid_evidence(quota, quota / 2 - 1, MIN_HOST_FREE_SPACE_BYTES).is_err());
        assert!(
            valid_evidence(
                quota + QUOTA_PROBE_EXTRA_WRITE_BYTES + 1,
                quota,
                MIN_HOST_FREE_SPACE_BYTES
            )
            .is_err()
        );
        assert!(valid_evidence(quota / 2, quota, MIN_HOST_FREE_SPACE_BYTES - 1).is_err());
    }

    #[test]
    fn python_runtime_mismatch_does_not_close_quota_but_cannot_attest_skill_runtime() {
        let quota = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let validate_runtime_marker = |runtime_attested: bool, configured: bool| {
            let marker = format!(
                "HAVEN_STORAGE_QUOTA_ATTESTED_V3:{}:{}:0\r\n",
                quota / 2,
                u8::from(runtime_attested)
            );
            validate_probe_evidence(
                quota,
                ProbeProcessEvidence {
                    exit_code: 0,
                    stdout: marker.as_bytes(),
                    stderr: &[],
                    stdout_truncated: false,
                    stderr_truncated: false,
                },
                ScratchGrowthEvidence {
                    physical_size_before: 1,
                    physical_size_after: quota / 2 + 1,
                },
                host_space(MIN_HOST_FREE_SPACE_BYTES),
                configured,
                false,
            )
        };

        let mismatch = validate_runtime_marker(false, true)
            .expect("a missing/mismatched Python binary must not invalidate quota evidence");
        assert!(!mismatch.skill_python_runtime_attested);
        assert!(
            validate_runtime_marker(true, true)
                .unwrap()
                .skill_python_runtime_attested
        );
        assert!(validate_runtime_marker(true, false).is_err());
    }

    #[test]
    fn mcp_node_runtime_mismatch_does_not_close_quota_but_cannot_attest_mcp_runtime() {
        let quota = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let validate_runtime_marker = |runtime_attested: bool, configured: bool| {
            let marker = format!(
                "HAVEN_STORAGE_QUOTA_ATTESTED_V3:{}:0:{}\r\n",
                quota / 2,
                u8::from(runtime_attested)
            );
            validate_probe_evidence(
                quota,
                ProbeProcessEvidence {
                    exit_code: 0,
                    stdout: marker.as_bytes(),
                    stderr: &[],
                    stdout_truncated: false,
                    stderr_truncated: false,
                },
                ScratchGrowthEvidence {
                    physical_size_before: 1,
                    physical_size_after: quota / 2 + 1,
                },
                host_space(MIN_HOST_FREE_SPACE_BYTES),
                false,
                configured,
            )
        };

        let mismatch = validate_runtime_marker(false, true)
            .expect("a missing/mismatched Node binary must not invalidate quota evidence");
        assert!(!mismatch.mcp_node_runtime_attested);
        assert!(
            validate_runtime_marker(true, true)
                .unwrap()
                .mcp_node_runtime_attested
        );
        assert!(validate_runtime_marker(true, false).is_err());
    }

    #[test]
    fn quota_probe_rejects_unexpected_output_or_process_state() {
        let quota = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let stdout = format!("HAVEN_STORAGE_QUOTA_ATTESTED_V3:{}:0:0\r\n", quota / 2);
        let evidence = |exit_code, stderr: &[u8], stdout_truncated, stderr_truncated| {
            validate_probe_evidence(
                quota,
                ProbeProcessEvidence {
                    exit_code,
                    stdout: stdout.as_bytes(),
                    stderr,
                    stdout_truncated,
                    stderr_truncated,
                },
                ScratchGrowthEvidence {
                    physical_size_before: 1,
                    physical_size_after: quota,
                },
                host_space(MIN_HOST_FREE_SPACE_BYTES),
                false,
                false,
            )
        };
        assert!(evidence(0, &[], false, false).is_ok());
        assert!(evidence(1, &[], false, false).is_err());
        assert!(evidence(0, b"warning", false, false).is_err());
        assert!(evidence(0, &[], true, false).is_err());
        assert!(evidence(0, &[], false, true).is_err());
        assert!(parse_probe_output(b"HAVEN_STORAGE_QUOTA_ATTESTED_V3:1:0:0\nextra\n").is_err());
        assert!(parse_probe_output(b"HAVEN_STORAGE_QUOTA_ATTESTED_V3:-1:0:0\n").is_err());
        assert!(parse_probe_output(b"HAVEN_STORAGE_QUOTA_ATTESTED_V3:1:2:0\n").is_err());
        assert!(parse_probe_output(b"HAVEN_STORAGE_QUOTA_ATTESTED_V1:1\n").is_err());
    }

    #[test]
    fn quota_probe_checks_host_headroom_before_it_writes() {
        let quota = WORKSPACE_WRITE_STORAGE_QUOTA_BYTES;
        let required = quota + MIN_HOST_FREE_SPACE_BYTES;
        assert!(validate_host_space_before(quota, host_space(required)).is_ok());
        assert!(validate_host_space_before(quota, host_space(required - 1)).is_err());
        assert!(
            validate_host_space_before(
                quota,
                HostSpace {
                    available_to_service: required,
                    total_free: required - 1
                }
            )
            .is_err()
        );
    }
}
