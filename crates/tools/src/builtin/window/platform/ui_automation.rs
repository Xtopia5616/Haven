use serde_json::Value;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use super::window_list::{
    find_window_by_title, hwnd_from_window_id, visible_window_title, window_id,
};

fn control_type_name(id: i32) -> &'static str {
    use windows::Win32::UI::Accessibility::*;
    // Match against known UIA control type ids.
    if id == UIA_ButtonControlTypeId.0 {
        "Button"
    } else if id == UIA_CalendarControlTypeId.0 {
        "Calendar"
    } else if id == UIA_CheckBoxControlTypeId.0 {
        "CheckBox"
    } else if id == UIA_ComboBoxControlTypeId.0 {
        "ComboBox"
    } else if id == UIA_EditControlTypeId.0 {
        "Edit"
    } else if id == UIA_HyperlinkControlTypeId.0 {
        "Hyperlink"
    } else if id == UIA_ImageControlTypeId.0 {
        "Image"
    } else if id == UIA_ListItemControlTypeId.0 {
        "ListItem"
    } else if id == UIA_ListControlTypeId.0 {
        "List"
    } else if id == UIA_MenuControlTypeId.0 {
        "Menu"
    } else if id == UIA_MenuBarControlTypeId.0 {
        "MenuBar"
    } else if id == UIA_MenuItemControlTypeId.0 {
        "MenuItem"
    } else if id == UIA_ProgressBarControlTypeId.0 {
        "ProgressBar"
    } else if id == UIA_RadioButtonControlTypeId.0 {
        "RadioButton"
    } else if id == UIA_ScrollBarControlTypeId.0 {
        "ScrollBar"
    } else if id == UIA_SliderControlTypeId.0 {
        "Slider"
    } else if id == UIA_SpinnerControlTypeId.0 {
        "Spinner"
    } else if id == UIA_StatusBarControlTypeId.0 {
        "StatusBar"
    } else if id == UIA_TabControlTypeId.0 {
        "Tab"
    } else if id == UIA_TabItemControlTypeId.0 {
        "TabItem"
    } else if id == UIA_TextControlTypeId.0 {
        "Text"
    } else if id == UIA_ToolBarControlTypeId.0 {
        "ToolBar"
    } else if id == UIA_ToolTipControlTypeId.0 {
        "ToolTip"
    } else if id == UIA_TreeControlTypeId.0 {
        "Tree"
    } else if id == UIA_TreeItemControlTypeId.0 {
        "TreeItem"
    } else if id == UIA_CustomControlTypeId.0 {
        "Custom"
    } else if id == UIA_GroupControlTypeId.0 {
        "Group"
    } else if id == UIA_ThumbControlTypeId.0 {
        "Thumb"
    } else if id == UIA_DataGridControlTypeId.0 {
        "DataGrid"
    } else if id == UIA_DataItemControlTypeId.0 {
        "DataItem"
    } else if id == UIA_DocumentControlTypeId.0 {
        "Document"
    } else if id == UIA_SplitButtonControlTypeId.0 {
        "SplitButton"
    } else if id == UIA_WindowControlTypeId.0 {
        "Window"
    } else if id == UIA_PaneControlTypeId.0 {
        "Pane"
    } else if id == UIA_HeaderControlTypeId.0 {
        "Header"
    } else if id == UIA_HeaderItemControlTypeId.0 {
        "HeaderItem"
    } else if id == UIA_TableControlTypeId.0 {
        "Table"
    } else if id == UIA_TitleBarControlTypeId.0 {
        "TitleBar"
    } else if id == UIA_SeparatorControlTypeId.0 {
        "Separator"
    } else {
        "Unknown"
    }
}

fn is_interactive_control(id: i32) -> bool {
    use windows::Win32::UI::Accessibility::*;
    [
        UIA_ButtonControlTypeId.0,
        UIA_CheckBoxControlTypeId.0,
        UIA_ComboBoxControlTypeId.0,
        UIA_EditControlTypeId.0,
        UIA_HyperlinkControlTypeId.0,
        UIA_ListItemControlTypeId.0,
        UIA_MenuItemControlTypeId.0,
        UIA_RadioButtonControlTypeId.0,
        UIA_SliderControlTypeId.0,
        UIA_SpinnerControlTypeId.0,
        UIA_SplitButtonControlTypeId.0,
        UIA_TabItemControlTypeId.0,
        UIA_TreeItemControlTypeId.0,
        UIA_DataItemControlTypeId.0,
        UIA_ScrollBarControlTypeId.0,
        UIA_ThumbControlTypeId.0,
        UIA_CalendarControlTypeId.0,
        UIA_DocumentControlTypeId.0,
    ]
    .contains(&id)
}

fn ui_automation_target_hwnd(window_id: Option<&str>, title: Option<&str>) -> anyhow::Result<HWND> {
    let target_hwnd = if let Some(window_id) = window_id.filter(|s| !s.trim().is_empty()) {
        hwnd_from_window_id(window_id)?
    } else if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
        let hwnd = find_window_by_title(t.trim())?;
        if hwnd.is_null() {
            anyhow::bail!("no window found matching '{}'", t);
        }
        hwnd
    } else {
        unsafe { GetForegroundWindow() }
    };
    if target_hwnd.is_null() {
        anyhow::bail!("no target window available for UI Automation");
    }
    Ok(target_hwnd)
}

fn element_token(
    hwnd: HWND,
    automation_id: &str,
    name: &str,
    control_type: &str,
    tree_index: usize,
) -> String {
    let prefix = window_id(hwnd);
    if automation_id.trim().is_empty() {
        // Include descriptive identity as well as the ordinal. The
        // ordinal disambiguates duplicate controls; the name/type guard
        // makes a stale token fail instead of silently clicking a new
        // control that moved into the same position.
        format!("uia:{prefix}:index:{tree_index}:type:{control_type}:name:{name}")
    } else {
        format!("uia:{prefix}:automation:{}", automation_id.trim())
    }
}

fn ui_automation_element(
    query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<(
    windows::Win32::UI::Accessibility::IUIAutomationElement,
    super::super::ui_automation::UiElementTarget,
)> {
    use windows::Win32::Foundation::HWND as WinHwnd;
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Accessibility::*;

    if let Some(control_type) = query.control_type.as_deref()
        && !super::super::ui_automation::is_known_ui_control_type(control_type)
    {
        anyhow::bail!("control_type must be one of the supported UI Automation control type names");
    }

    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let target_hwnd =
        ui_automation_target_hwnd(query.window_id.as_deref(), query.title.as_deref())?;
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let root = unsafe { automation.ElementFromHandle(WinHwnd(target_hwnd))? };
    let condition = unsafe { automation.CreateTrueCondition()? };
    let array = unsafe { root.FindAll(TreeScope_Descendants, &condition)? };
    let len = unsafe { array.Length()? }.max(0) as usize;

    let mut matches = Vec::new();
    for i in 0..len {
        let element = match unsafe { array.GetElement(i as i32) } {
            Ok(element) => element,
            Err(_) => continue,
        };
        let control_type = match unsafe { element.CurrentControlType() } {
            Ok(control_type) => control_type.0,
            Err(_) => continue,
        };
        if !is_interactive_control(control_type) {
            continue;
        }
        let control_type_name = control_type_name(control_type);
        if query
            .control_type
            .as_deref()
            .is_some_and(|wanted| wanted != control_type_name)
        {
            continue;
        }
        let name = unsafe { element.CurrentName() }
            .ok()
            .map(|value| value.to_string())
            .unwrap_or_default();
        let automation_id = unsafe { element.CurrentAutomationId() }
            .ok()
            .map(|value| value.to_string())
            .unwrap_or_default();
        let token = element_token(target_hwnd, &automation_id, &name, control_type_name, i);
        if query
            .element_token
            .as_deref()
            .is_some_and(|wanted| wanted != token)
        {
            continue;
        }
        if query.element_token.is_none() && name != query.name {
            continue;
        }
        let enabled = unsafe { element.CurrentIsEnabled() }
            .ok()
            .map(|value| value.as_bool())
            .unwrap_or(false);
        let bounds = unsafe { element.CurrentBoundingRectangle() }.ok();
        let bounds_valid = bounds
            .as_ref()
            .is_some_and(|bounds| bounds.right > bounds.left && bounds.bottom > bounds.top);
        let (center_x, center_y) = bounds
            .filter(|bounds| bounds.right > bounds.left && bounds.bottom > bounds.top)
            .map(|bounds| {
                (
                    i64::from(bounds.left) + i64::from(bounds.right - bounds.left) / 2,
                    i64::from(bounds.top) + i64::from(bounds.bottom - bounds.top) / 2,
                )
            })
            .unwrap_or((0, 0));
        matches.push((
            element,
            super::super::ui_automation::UiElementTarget {
                window_id: window_id(target_hwnd),
                element_token: token,
                name,
                control_type: control_type_name.to_owned(),
                index: matches.len(),
                center_x,
                center_y,
            },
            enabled,
            bounds_valid,
        ));
    }

    if matches.is_empty() {
        let type_hint = query
            .control_type
            .as_deref()
            .map(|value| format!(" with control_type '{value}'"))
            .unwrap_or_default();
        anyhow::bail!(
            "no UI Automation control named '{}'{} was found",
            query.name,
            type_hint
        );
    }

    let selected_index = match query.index {
        Some(index) if index >= matches.len() => {
            anyhow::bail!(
                "UI Automation control '{}' has {} matches; index {} is out of range",
                query.name,
                matches.len(),
                index
            )
        }
        Some(index) => index,
        None if matches.len() == 1 => 0,
        None => {
            anyhow::bail!(
                "UI Automation control '{}' matched {} elements; provide control_type or a zero-based index",
                query.name,
                matches.len()
            )
        }
    };

    let (element, target, enabled, has_bounds) = matches.swap_remove(selected_index);
    if !enabled {
        anyhow::bail!(
            "UI Automation control '{}' is disabled and cannot receive input",
            query.name
        );
    }
    if !has_bounds {
        anyhow::bail!(
            "UI Automation control '{}' has no usable screen bounds",
            query.name
        );
    }
    Ok((element, target))
}

pub(crate) fn resolve_ui_element(
    query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    ui_automation_element(query).map(|(_, target)| target)
}

pub(crate) fn focus_ui_element(
    query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    let (element, target) = ui_automation_element(query)?;
    unsafe { element.SetFocus()? };
    Ok(target)
}

pub(crate) fn invoke_ui_element(
    query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    use windows::Win32::UI::Accessibility::{IUIAutomationInvokePattern, UIA_InvokePatternId};
    let (element, target) = ui_automation_element(query)?;
    let pattern: IUIAutomationInvokePattern =
        unsafe { element.GetCurrentPatternAs(UIA_InvokePatternId)? };
    unsafe { pattern.Invoke()? };
    Ok(target)
}

pub(crate) fn set_ui_element_value(
    query: &super::super::ui_automation::UiElementQuery,
    value: &str,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    use windows::Win32::UI::Accessibility::{IUIAutomationValuePattern, UIA_ValuePatternId};
    let (element, target) = ui_automation_element(query)?;
    let pattern: IUIAutomationValuePattern =
        unsafe { element.GetCurrentPatternAs(UIA_ValuePatternId)? };
    let value = windows::core::BSTR::from(value);
    unsafe { pattern.SetValue(&value)? };
    Ok(target)
}

pub(crate) fn toggle_ui_element(
    query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    use windows::Win32::UI::Accessibility::{IUIAutomationTogglePattern, UIA_TogglePatternId};
    let (element, target) = ui_automation_element(query)?;
    let pattern: IUIAutomationTogglePattern =
        unsafe { element.GetCurrentPatternAs(UIA_TogglePatternId)? };
    unsafe { pattern.Toggle()? };
    Ok(target)
}

pub(crate) fn select_ui_element(
    query: &super::super::ui_automation::UiElementQuery,
) -> anyhow::Result<super::super::ui_automation::UiElementTarget> {
    use windows::Win32::UI::Accessibility::{
        IUIAutomationSelectionItemPattern, UIA_SelectionItemPatternId,
    };
    let (element, target) = ui_automation_element(query)?;
    let pattern: IUIAutomationSelectionItemPattern =
        unsafe { element.GetCurrentPatternAs(UIA_SelectionItemPatternId)? };
    unsafe { pattern.Select()? };
    Ok(target)
}

/// Enumerate interactive UI Automation elements for the foreground window
/// (or the first window whose title contains `title`).
pub(crate) fn enumerate_ui_tree(
    target_window_id: Option<&str>,
    title: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    use windows::Win32::Foundation::HWND as WinHwnd;
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Accessibility::*;

    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

    let target_hwnd: HWND =
        if let Some(window_id) = target_window_id.filter(|s| !s.trim().is_empty()) {
            hwnd_from_window_id(window_id)?
        } else if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
            let hwnd = find_window_by_title(t.trim())?;
            if hwnd.is_null() {
                anyhow::bail!("no window found matching '{}'", t);
            }
            hwnd
        } else {
            unsafe { GetForegroundWindow() }
        };
    if target_hwnd.is_null() {
        anyhow::bail!("no target window for ui_tree");
    }

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let element = unsafe { automation.ElementFromHandle(WinHwnd(target_hwnd))? };
    let condition = unsafe { automation.CreateTrueCondition()? };
    let array = unsafe { element.FindAll(TreeScope_Descendants, &condition)? };
    let len = unsafe { array.Length()? }.max(0) as usize;
    let target_title = visible_window_title(target_hwnd).unwrap_or_default();

    let mut out = Vec::new();
    for i in 0..len {
        if out.len() >= super::super::ui_automation::UI_TREE_CAP {
            break;
        }
        let el = match unsafe { array.GetElement(i as i32) } {
            Ok(e) => e,
            Err(_) => continue,
        };
        let control_type = match unsafe { el.CurrentControlType() } {
            Ok(ct) => ct.0,
            Err(_) => continue,
        };
        if !is_interactive_control(control_type) {
            continue;
        }
        let name = unsafe { el.CurrentName() }
            .ok()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let automation_id = unsafe { el.CurrentAutomationId() }
            .ok()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let control_name = control_type_name(control_type);
        let token = element_token(target_hwnd, &automation_id, &name, control_name, i);
        let enabled = unsafe { el.CurrentIsEnabled() }
            .ok()
            .map(|b| b.as_bool())
            .unwrap_or(false);
        let bounds = unsafe { el.CurrentBoundingRectangle() }.ok();
        let bounds_json = match bounds {
            Some(r) => serde_json::json!({
                "left": r.left,
                "top": r.top,
                "right": r.right,
                "bottom": r.bottom,
            }),
            None => serde_json::json!({
                "left": 0, "top": 0, "right": 0, "bottom": 0
            }),
        };
        out.push(serde_json::json!({
            "window_id": window_id(target_hwnd),
            "window_title": target_title,
            "element_token": token,
            "automation_id": automation_id,
            "name": name,
            "control_type": control_name,
            "bounds": bounds_json,
            "enabled": enabled,
        }));
    }

    Ok(out)
}

/// Early-exit name scan for `wait`/`ui_text` — no JSON materialization.
pub(crate) fn any_ui_name_contains(title: Option<&str>, needle: &str) -> anyhow::Result<bool> {
    use windows::Win32::Foundation::HWND as WinHwnd;
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Accessibility::*;

    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

    let target_hwnd: HWND = if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
        let hwnd = find_window_by_title(t.trim())?;
        if hwnd.is_null() {
            return Ok(false);
        }
        hwnd
    } else {
        unsafe { GetForegroundWindow() }
    };
    if target_hwnd.is_null() {
        return Ok(false);
    }

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let element = unsafe { automation.ElementFromHandle(WinHwnd(target_hwnd))? };
    let condition = unsafe { automation.CreateTrueCondition()? };
    let array = unsafe { element.FindAll(TreeScope_Descendants, &condition)? };
    let len = unsafe { array.Length()? }.max(0) as i32;
    for i in 0..len {
        let el = match unsafe { array.GetElement(i) } {
            Ok(e) => e,
            Err(_) => continue,
        };
        let control_type = match unsafe { el.CurrentControlType() } {
            Ok(ct) => ct.0,
            Err(_) => continue,
        };
        if !is_interactive_control(control_type) {
            continue;
        }
        let name = unsafe { el.CurrentName() }
            .ok()
            .map(|b| b.to_string())
            .unwrap_or_default();
        if name.contains(needle) {
            return Ok(true);
        }
    }
    Ok(false)
}
