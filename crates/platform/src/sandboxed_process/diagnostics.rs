//! Safe, model-visible diagnostics for MXC process isolation.
//!
//! MXC error codes are a closed SDK vocabulary. Probe warnings are currently
//! free-form strings, so only the documented host-preparation markers are
//! interpreted as stable categories; unknown warnings remain bounded and
//! sanitized for display.

use anyhow::Error as AnyhowError;
use mxc_sdk::v1::{Error as MxcError, ErrorCode};

const MAX_VISIBLE_WARNINGS: usize = 4;

pub(super) fn format_mxc_failure(stage: &'static str, error: &AnyhowError) -> String {
    let diagnostic = haven_common::error::sanitize_error_text(&error.to_string());
    if let Some(error) = error.downcast_ref::<MxcError>() {
        let (category, description) = error_category(error.code);
        let message = haven_common::error::sanitize_error_text(&error.message);
        let remediation = error
            .remediation
            .as_deref()
            .map(haven_common::error::sanitize_error_text)
            .filter(|value| !value.is_empty())
            .map(|value| format!(" MXC 建议：{value}"))
            .unwrap_or_default();

        tracing::warn!(
            stage,
            category,
            mxc_code = error.code.as_str(),
            operation = error.operation.as_deref().unwrap_or("unknown"),
            native_code = error.native_code.as_deref().unwrap_or("unknown"),
            diagnostic = %diagnostic,
            "MXC isolated shell attempt failed"
        );
        format!(
            "{stage}失败 [{category} / {}]（{description}）：{message}{remediation}",
            error.code.as_str()
        )
    } else {
        tracing::warn!(
            stage,
            category = "mxc.unknown_api_error",
            diagnostic = %diagnostic,
            "MXC isolated shell attempt failed"
        );
        format!("{stage}失败 [mxc.unknown_api_error]：{diagnostic}")
    }
}

fn error_category(code: ErrorCode) -> (&'static str, &'static str) {
    match code {
        ErrorCode::MalformedRequest | ErrorCode::MalformedId => {
            ("mxc.invalid_request", "MXC 请求格式无效")
        }
        ErrorCode::UnsupportedContainment | ErrorCode::UnsupportedPhase => (
            "mxc.unsupported_capability",
            "当前主机或 MXC 不支持该隔离能力",
        ),
        ErrorCode::BackendUnavailable => ("mxc.backend_unavailable", "MXC 隔离后端不可用"),
        ErrorCode::StaleId
        | ErrorCode::NotProvisioned
        | ErrorCode::NotStarted
        | ErrorCode::AlreadyStarted
        | ErrorCode::AlreadyStopped => ("mxc.lifecycle_state", "MXC 进程状态不匹配"),
        ErrorCode::PolicyValidation => ("mxc.policy_rejected", "MXC 拒绝该隔离策略"),
        ErrorCode::BackendError => ("mxc.backend_failure", "MXC 执行隔离操作失败"),
    }
}

pub(super) fn format_warnings(warnings: &[String]) -> String {
    let mut formatted = Vec::new();
    let mut categories = Vec::new();

    for warning in warnings {
        let (category, message) = classify_warning(warning);
        if category != "mxc.warning" {
            if categories.contains(&category) {
                continue;
            }
            categories.push(category);
        }
        formatted.push(message);
    }

    let omitted_count = formatted.len().saturating_sub(MAX_VISIBLE_WARNINGS);
    formatted.truncate(MAX_VISIBLE_WARNINGS);
    if omitted_count > 0 {
        formatted.push(format!("另有 {omitted_count} 条提示已写入应用日志。"));
    }
    formatted.join("；")
}

pub(super) fn log_probe_warnings(warnings: &[String]) {
    for warning in warnings {
        tracing::warn!(
            diagnostic = %haven_common::error::sanitize_error_text(warning),
            "MXC process-container probe warning"
        );
    }
}

fn classify_warning(warning: &str) -> (&'static str, String) {
    let normalized = warning.to_ascii_lowercase();
    if normalized.contains("prepare-system-drive") {
        return (
            "mxc.host_prep.system_drive_acl_missing",
            "[mxc.host_prep.system_drive_acl_missing] 系统盘根目录缺少 AppContainer 元数据读取 ACL，cmd.exe、PowerShell 或 Node 可能因此启动失败。修复：以管理员身份运行 `wxc-host-prep.exe prepare-system-drive`（一次性准备）。".to_owned(),
        );
    }
    if normalized.contains("prepare-null-device")
        || normalized.contains("\\device\\null")
        || normalized.contains("nul device")
    {
        return (
            "mxc.host_prep.null_device_acl_missing",
            "[mxc.host_prep.null_device_acl_missing] AppContainer 可能无法打开 Windows NUL 设备（`\\Device\\Null`）。修复：以管理员身份运行 `wxc-host-prep.exe prepare-null-device`；Windows 重启后需重新准备。".to_owned(),
        );
    }

    (
        "mxc.warning",
        format!(
            "[mxc.warning] {}",
            haven_common::error::sanitize_error_text(warning)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{classify_warning, format_mxc_failure, format_warnings};
    use mxc_sdk::v1::{Error, ErrorCode};

    #[test]
    fn mxc_errors_show_stage_stable_category_and_safe_remediation() {
        let mut error = Error::new(
            ErrorCode::BackendUnavailable,
            r"backend failed at C:\private\host",
        );
        error.remediation = Some("Run the MXC repair command.".into());
        let error = anyhow::Error::new(error);

        let formatted = format_mxc_failure("启动", &error);

        assert!(formatted.contains("启动失败"));
        assert!(formatted.contains("mxc.backend_unavailable"));
        assert!(formatted.contains("backend_unavailable"));
        assert!(formatted.contains("MXC 建议：Run the MXC repair command."));
        assert!(!formatted.contains(r"C:\private"));
    }

    #[test]
    fn system_drive_warning_has_a_stable_category_and_admin_action() {
        let (category, formatted) = classify_warning(
            "AppContainer + DACL tier selected: startup stats of C:\\ may fail. Run `wxc-host-prep prepare-system-drive` (elevated).",
        );

        assert_eq!(category, "mxc.host_prep.system_drive_acl_missing");
        assert!(formatted.contains("wxc-host-prep.exe prepare-system-drive"));
        assert!(formatted.contains("管理员身份"));
        assert!(!formatted.contains(r"C:\"));
    }

    #[test]
    fn null_device_warning_has_a_stable_category_and_reboot_note() {
        let (category, formatted) =
            classify_warning("prepare-null-device required for \\Device\\Null");

        assert_eq!(category, "mxc.host_prep.null_device_acl_missing");
        assert!(formatted.contains("wxc-host-prep.exe prepare-null-device"));
        assert!(formatted.contains("重启后需重新准备"));
    }

    #[test]
    fn warning_output_deduplicates_known_categories_and_bounds_unknowns() {
        let formatted = format_warnings(&[
            "prepare-system-drive".into(),
            "prepare-system-drive is still recommended".into(),
            "unknown warning one".into(),
            "unknown warning two".into(),
            "unknown warning three".into(),
            "unknown warning four".into(),
            "unknown warning five".into(),
        ]);

        assert_eq!(
            formatted
                .matches("mxc.host_prep.system_drive_acl_missing")
                .count(),
            1
        );
        assert!(formatted.contains("另有 2 条提示已写入应用日志"));
        assert!(!formatted.contains("unknown warning five"));
    }
}
