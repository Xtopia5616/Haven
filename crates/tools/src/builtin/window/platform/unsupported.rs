use serde_json::Value;

pub(crate) fn enumerate_windows(_filter_pid: Option<u32>) -> anyhow::Result<Vec<Value>> {
    Ok(Vec::new())
}

pub(crate) fn get_foreground_window_info() -> anyhow::Result<Value> {
    Ok(serde_json::json!({"available": false, "note": "window operations require Windows"}))
}

pub(crate) fn focus_window(
    _window_id: Option<&str>,
    _title: Option<&str>,
    _pid: Option<u32>,
) -> anyhow::Result<()> {
    anyhow::bail!("window operations require Windows")
}

pub(crate) fn close_window(
    _window_id: Option<&str>,
    _title: Option<&str>,
    _pid: Option<u32>,
) -> anyhow::Result<()> {
    anyhow::bail!("window operations require Windows")
}

pub(crate) fn capture_screen(_path: std::path::PathBuf) -> anyhow::Result<Value> {
    anyhow::bail!("screenshot requires Windows")
}

pub(crate) fn any_title_contains(_needle: &str) -> anyhow::Result<bool> {
    Ok(false)
}

pub(crate) fn foreground_title_contains(_needle: &str) -> anyhow::Result<bool> {
    Ok(false)
}

pub(crate) fn enumerate_ui_tree(
    _window_id: Option<&str>,
    _title: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    anyhow::bail!("ui_tree requires Windows")
}

pub(crate) fn resolve_ui_element(
    _query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    anyhow::bail!("UI Automation element input requires Windows")
}

pub(crate) fn focus_ui_element(
    _query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    anyhow::bail!("UI Automation element input requires Windows")
}

pub(crate) fn invoke_ui_element(
    _query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    anyhow::bail!("UI Automation element input requires Windows")
}

pub(crate) fn set_ui_element_value(
    _query: &super::super::ui_automation::UiElementQuery,
    _value: &str,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    anyhow::bail!("UI Automation element input requires Windows")
}

pub(crate) fn toggle_ui_element(
    _query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    anyhow::bail!("UI Automation element input requires Windows")
}

pub(crate) fn select_ui_element(
    _query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    anyhow::bail!("UI Automation element input requires Windows")
}

pub(crate) fn any_ui_name_contains(_title: Option<&str>, _needle: &str) -> anyhow::Result<bool> {
    Ok(false)
}
