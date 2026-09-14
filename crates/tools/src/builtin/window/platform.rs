#[cfg(windows)]
#[path = "platform/screenshot.rs"]
mod screenshot;
#[cfg(windows)]
#[path = "platform/ui_automation.rs"]
mod ui_automation;
#[cfg(not(windows))]
#[path = "platform/unsupported.rs"]
mod unsupported;
#[cfg(windows)]
#[path = "platform/window_list.rs"]
mod window_list;

#[cfg(windows)]
pub(super) use screenshot::capture_screen;
#[cfg(windows)]
pub(super) use ui_automation::{
    any_ui_name_contains, enumerate_ui_tree, focus_ui_element, invoke_ui_element,
    resolve_ui_element, select_ui_element, set_ui_element_value, toggle_ui_element,
};
#[cfg(windows)]
pub(super) use window_list::{
    any_title_contains, close_window, enumerate_windows, focus_window, foreground_title_contains,
    get_foreground_window_info,
};

#[cfg(not(windows))]
pub(super) use unsupported::{
    any_title_contains, any_ui_name_contains, capture_screen, close_window, enumerate_ui_tree,
    enumerate_windows, focus_ui_element, focus_window, foreground_title_contains,
    get_foreground_window_info, invoke_ui_element, resolve_ui_element, select_ui_element,
    set_ui_element_value, toggle_ui_element,
};
