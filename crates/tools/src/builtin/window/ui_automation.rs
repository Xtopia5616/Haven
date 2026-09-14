use super::WindowParams;
use super::platform;

pub(super) const UI_TREE_CAP: usize = 100;

/// Canonical UI Automation control type names accepted by element input.
/// These names intentionally mirror the existing `window.ui_tree` output and
/// are matched case-sensitively so a request cannot silently broaden its
/// target.
pub(crate) const UIA_CONTROL_TYPE_NAMES: &[&str] = &[
    "Button",
    "Calendar",
    "CheckBox",
    "ComboBox",
    "Edit",
    "Hyperlink",
    "Image",
    "ListItem",
    "List",
    "Menu",
    "MenuBar",
    "MenuItem",
    "ProgressBar",
    "RadioButton",
    "ScrollBar",
    "Slider",
    "Spinner",
    "StatusBar",
    "Tab",
    "TabItem",
    "Text",
    "ToolBar",
    "ToolTip",
    "Tree",
    "TreeItem",
    "Custom",
    "Group",
    "Thumb",
    "DataGrid",
    "DataItem",
    "Document",
    "SplitButton",
    "Window",
    "Pane",
    "Header",
    "HeaderItem",
    "Table",
    "TitleBar",
    "Separator",
];

#[derive(Debug, Clone)]
pub(crate) struct UiElementQuery {
    pub window_id: Option<String>,
    pub title: Option<String>,
    pub name: String,
    pub control_type: Option<String>,
    pub index: Option<usize>,
    pub element_token: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct UiElementTarget {
    pub window_id: String,
    pub element_token: String,
    pub name: String,
    pub control_type: String,
    pub index: usize,
    pub center_x: i64,
    pub center_y: i64,
}

pub(crate) fn is_known_ui_control_type(control_type: &str) -> bool {
    UIA_CONTROL_TYPE_NAMES.contains(&control_type)
}

pub(crate) fn resolve_ui_element(query: &UiElementQuery) -> anyhow::Result<UiElementTarget> {
    platform::resolve_ui_element(query)
}

pub(crate) fn focus_ui_element(query: &UiElementQuery) -> anyhow::Result<UiElementTarget> {
    platform::focus_ui_element(query)
}

pub(crate) fn invoke_ui_element(query: &UiElementQuery) -> anyhow::Result<UiElementTarget> {
    platform::invoke_ui_element(query)
}

pub(crate) fn set_ui_element_value(
    query: &UiElementQuery,
    value: &str,
) -> anyhow::Result<UiElementTarget> {
    platform::set_ui_element_value(query, value)
}

pub(crate) fn toggle_ui_element(query: &UiElementQuery) -> anyhow::Result<UiElementTarget> {
    platform::toggle_ui_element(query)
}

pub(crate) fn select_ui_element(query: &UiElementQuery) -> anyhow::Result<UiElementTarget> {
    platform::select_ui_element(query)
}

pub(super) fn window_element_query(params: &WindowParams) -> anyhow::Result<UiElementQuery> {
    let element_token = params
        .element_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let name = params
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_owned();
    if element_token.is_none() && name.is_empty() {
        anyhow::bail!("element_token or name is required for UI Automation operations");
    }
    let control_type = params
        .control_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if let Some(control_type) = control_type.as_deref()
        && !is_known_ui_control_type(control_type)
    {
        anyhow::bail!("control_type must be one of the supported UI Automation control type names");
    }
    Ok(UiElementQuery {
        window_id: params.window_id.clone(),
        title: params.title.clone(),
        name,
        control_type,
        index: params.index,
        element_token,
    })
}
