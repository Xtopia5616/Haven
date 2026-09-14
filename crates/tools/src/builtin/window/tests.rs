use super::*;
use crate::ManagedAssetRegistry;
use crate::Tool;
use crate::builtin::media::MediaTool;
use haven_common::types::RiskLevel;
use serde_json::json;

fn tool() -> WindowTool {
    let registry = ManagedAssetRegistry::default();
    let media = MediaTool::new(None, registry.clone(), 8 * 1024 * 1024, 60, 32_000);
    let capture_root = std::env::temp_dir().join(format!(
        "haven-window-test-{}",
        haven_common::types::new_id("file")
    ));
    WindowTool::new(registry)
        .with_media_tool(Arc::new(media.with_capabilities(true, false)))
        .with_capture_root(capture_root)
}

#[test]
fn test_window_tool_name() {
    assert_eq!(tool().name(), "window");
}

#[test]
fn test_window_tool_risk_level() {
    let t = tool();
    assert_eq!(t.risk_level(&json!({"operation": "list"})), RiskLevel::Low);
    assert_eq!(
        t.risk_level(&json!({"operation": "focus"})),
        RiskLevel::Medium
    );
    assert_eq!(
        t.risk_level(&json!({"operation": "close"})),
        RiskLevel::High
    );
    assert_eq!(t.risk_level(&json!({"operation": "ocr"})), RiskLevel::High);
    assert_eq!(
        t.risk_level(&json!({"operation": "ui_tree"})),
        RiskLevel::Low
    );
    assert_eq!(t.risk_level(&json!({"operation": "wait"})), RiskLevel::Low);
}

#[test]
fn test_window_tool_input_schema() {
    let schema = tool().input_schema();
    let ops = schema["properties"]["operation"]["enum"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = ops.iter().map(|v| v.as_str().unwrap()).collect();
    for expected in [
        "list",
        "foreground",
        "focus",
        "close",
        "screenshot",
        "ui_tree",
        "observe",
        "invoke",
        "set_value",
        "toggle",
        "select",
        "wait",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
    assert!(
        schema["properties"]["condition"]["enum"]
            .as_array()
            .is_some()
    );
    assert!(
        tool()
            .validate_input(&json!({"operation": "focus", "pid": 1}))
            .is_ok()
    );
    assert!(
        tool()
            .validate_input(&json!({"operation": "close", "pid": 1}))
            .is_ok()
    );
    assert!(
        tool()
            .validate_input(&json!({
                "operation": "focus",
                "title": "Editor",
                "pid": 1
            }))
            .is_ok()
    );
    assert!(
        tool()
            .validate_input(&json!({
                "operation": "close",
                "title": "Editor",
                "pid": 1
            }))
            .is_ok()
    );
    assert!(
        tool()
            .validate_input(&json!({
                "operation": "screenshot",
                "path": "C:\\Temp\\shot.png"
            }))
            .is_err()
    );
}

#[tokio::test]
async fn test_window_execute_list() {
    let result = tool()
        .execute(json!({"operation": "list"}), CancellationToken::new())
        .await
        .unwrap();
    assert!(result.success);
    let windows = result.output["windows"].as_array().unwrap();
    for w in windows {
        assert!(w["title"].as_str().is_some());
        assert!(w["pid"].is_number());
    }
    assert!(result.output["count"].as_u64().unwrap() == windows.len() as u64);
}

#[tokio::test]
async fn test_window_execute_list_filtered_by_pid() {
    let result = tool()
        .execute(
            json!({"operation": "list", "pid": 99999999}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    let windows = result.output["windows"].as_array().unwrap();
    for w in windows {
        assert_eq!(w["pid"].as_u64().unwrap(), 99999999);
    }
}

#[tokio::test]
async fn test_window_execute_foreground() {
    let result = tool()
        .execute(json!({"operation": "foreground"}), CancellationToken::new())
        .await
        .unwrap();
    assert!(result.success);
    #[cfg(windows)]
    {
        assert!(result.output["hwnd"].is_number());
        assert!(result.output["title"].is_string());
        assert!(result.output["pid"].is_number());
    }
    #[cfg(not(windows))]
    {
        assert_eq!(result.output["available"], false);
    }
}

#[tokio::test]
async fn test_window_execute_focus_no_match() {
    let result = tool()
        .execute(
            json!({"operation": "focus", "title": "haven-test-no-such-window-xyz"}),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_window_execute_close_no_match() {
    let result = tool()
        .execute(
            json!({"operation": "close", "title": "haven-test-no-such-window-xyz"}),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_window_execute_focus_requires_target() {
    let result = tool()
        .execute(json!({"operation": "focus"}), CancellationToken::new())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_window_execute_unknown_operation() {
    let result = tool()
        .execute(json!({"operation": "bogus"}), CancellationToken::new())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_window_execute_cancelled() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = tool().execute(json!({"operation": "list"}), cancel).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_window_native_entry_lands_in_run() {
    let result = tool()
        .run(
            WindowParams {
                operation: Some(WindowOperation::Focus),
                title: Some("haven-test-no-such-window-xyz".into()),
                window_id: None,
                pid: None,
                condition: None,
                text: None,
                timeout_secs: None,
                element_token: None,
                name: None,
                control_type: None,
                index: None,
                value: None,
                ocr: None,
                session_id: None,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_window_ocr_without_router() {
    let result = tool()
        .execute(json!({"operation": "ocr"}), CancellationToken::new())
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error)
            if error.to_string().contains("BitBlt failed")
                || error.to_string().contains("screenshot requires Windows") =>
        {
            // CI and headless Windows sessions do not expose a capturable
            // desktop. The provider-unavailable branch is still covered
            // when a screen capture is available; this test must not turn
            // desktop availability into a workspace-wide test failure.
            return;
        }
        Err(error) => panic!("unexpected OCR setup failure: {error}"),
    };
    assert!(result.success);
    assert_eq!(result.output["available"], false);
    assert!(result.output["asset_id"].as_str().is_some());
    assert!(result.output["media"]["asset_id"].as_str().is_some());
    assert!(result.output.get("path").is_none());
}

#[tokio::test]
async fn test_window_ui_tree_rejects_missing_target() {
    let result = tool()
        .execute(
            json!({"operation": "ui_tree", "title": "haven-test-no-such-window-xyz"}),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_window_wait_title_timeout() {
    let result = tool()
        .execute(
            json!({
                "operation": "wait",
                "condition": "title_contains",
                "text": "haven-wait-no-such-title-xyz-999",
                "timeout_secs": 1
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output["timed_out"], true);
    assert_eq!(result.output["matched"], false);
    assert_eq!(result.output["waited"], true);
}

#[tokio::test]
async fn test_window_wait_requires_condition() {
    let err = tool()
        .execute(
            json!({"operation": "wait", "text": "x"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("condition"));
}
