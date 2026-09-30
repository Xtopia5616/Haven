//! Keep the main Windows window within the aspect ratios supported by the UI.

use std::mem::size_of;

use tauri::WebviewWindow;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, MINMAXINFO, WM_GETMINMAXINFO, WM_NCDESTROY, WM_SIZING,
    WMSZ_BOTTOM, WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT, WMSZ_LEFT, WMSZ_RIGHT, WMSZ_TOP, WMSZ_TOPLEFT,
    WMSZ_TOPRIGHT,
};

const SUBCLASS_ID: usize = 0x4841_5645;
const MIN_ASPECT_WIDTH: i32 = 2;
const MIN_ASPECT_HEIGHT: i32 = 3;
const MAX_ASPECT_WIDTH: i32 = 7;
const MAX_ASPECT_HEIGHT: i32 = 3;

#[derive(Clone, Copy)]
struct FrameInsets {
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AspectViolation {
    TooTall,
    TooWide,
}

/// Installs a native sizing hook on the main window. Tauri setup runs on the
/// window thread, which is required by `SetWindowSubclass`.
pub(crate) fn install(window: &WebviewWindow) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    // SAFETY: `hwnd` belongs to the live Tauri window, and setup executes on
    // that window's UI thread. The callback is static and holds no borrowed data.
    let installed = unsafe {
        SetWindowSubclass(
            hwnd.0 as HWND,
            Some(aspect_ratio_subclass_proc),
            SUBCLASS_ID,
            0,
        )
    };
    if installed == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

unsafe extern "system" fn aspect_ratio_subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    subclass_id: usize,
    _reference_data: usize,
) -> isize {
    if message == WM_SIZING
        && lparam != 0
        && let Some(insets) = unsafe { frame_insets(hwnd) }
    {
        // SAFETY: WM_SIZING supplies a live RECT pointer for the duration
        // of this synchronous window-message callback.
        let rect = unsafe { &mut *(lparam as *mut RECT) };
        constrain_sizing_rect(rect, wparam as u32, insets);
    }

    // Let the normal window procedure initialize maximize geometry before
    // narrowing it to the supported ratio for unusually shaped monitors.
    // SAFETY: forwarding the original message and parameters is the required
    // contract for a common-controls subclass procedure.
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };

    if message == WM_GETMINMAXINFO
        && lparam != 0
        && let Some(geometry) = unsafe { maximize_geometry(hwnd) }
    {
        // SAFETY: WM_GETMINMAXINFO supplies a live MINMAXINFO pointer for
        // the duration of this synchronous window-message callback.
        let info = unsafe { &mut *(lparam as *mut MINMAXINFO) };
        info.ptMaxSize.x = geometry.width;
        info.ptMaxSize.y = geometry.height;
        info.ptMaxPosition.x = geometry.x;
        info.ptMaxPosition.y = geometry.y;
    }

    if message == WM_NCDESTROY {
        // SAFETY: this removes only the subclass installed above for this HWND.
        unsafe { RemoveWindowSubclass(hwnd, Some(aspect_ratio_subclass_proc), subclass_id) };
    }

    result
}

unsafe fn frame_insets(hwnd: HWND) -> Option<FrameInsets> {
    let mut outer = RECT::default();
    let mut client = RECT::default();
    // SAFETY: both APIs write to valid local RECT values for the live HWND.
    if unsafe { GetWindowRect(hwnd, &mut outer) } == 0
        || unsafe { GetClientRect(hwnd, &mut client) } == 0
    {
        return None;
    }

    let outer_width = outer.right - outer.left;
    let outer_height = outer.bottom - outer.top;
    let client_width = client.right - client.left;
    let client_height = client.bottom - client.top;
    Some(FrameInsets {
        width: (outer_width - client_width).max(0),
        height: (outer_height - client_height).max(0),
    })
}

#[derive(Clone, Copy)]
struct MaximizeGeometry {
    width: i32,
    height: i32,
    x: i32,
    y: i32,
}

unsafe fn maximize_geometry(hwnd: HWND) -> Option<MaximizeGeometry> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    // SAFETY: `monitor` is returned by MonitorFromWindow and `info` is sized.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    let insets = unsafe { frame_insets(hwnd) }?;
    maximize_geometry_for_work_area(info.rcMonitor, info.rcWork, insets)
}

fn aspect_violation(width: i32, height: i32) -> Option<AspectViolation> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let scaled_width = i64::from(width) * i64::from(MIN_ASPECT_HEIGHT);
    let scaled_height = i64::from(height) * i64::from(MIN_ASPECT_WIDTH);
    if scaled_width < scaled_height {
        Some(AspectViolation::TooTall)
    } else if i64::from(width) * i64::from(MAX_ASPECT_HEIGHT)
        > i64::from(height) * i64::from(MAX_ASPECT_WIDTH)
    {
        Some(AspectViolation::TooWide)
    } else {
        None
    }
}

fn constrain_sizing_rect(rect: &mut RECT, edge: u32, insets: FrameInsets) -> bool {
    let outer_width = rect.right - rect.left;
    let outer_height = rect.bottom - rect.top;
    let client_width = (outer_width - insets.width).max(1);
    let client_height = (outer_height - insets.height).max(1);
    let Some(violation) = aspect_violation(client_width, client_height) else {
        return false;
    };

    match edge {
        WMSZ_LEFT | WMSZ_RIGHT => {
            let bounded_client_width = match violation {
                AspectViolation::TooTall => {
                    ceil_div(client_height * MIN_ASPECT_WIDTH, MIN_ASPECT_HEIGHT)
                }
                AspectViolation::TooWide => client_height * MAX_ASPECT_WIDTH / MAX_ASPECT_HEIGHT,
            };
            let bounded_outer_width = bounded_client_width + insets.width;
            if edge == WMSZ_LEFT {
                rect.left = rect.right - bounded_outer_width;
            } else {
                rect.right = rect.left + bounded_outer_width;
            }
        }
        WMSZ_TOP | WMSZ_BOTTOM | WMSZ_TOPLEFT | WMSZ_TOPRIGHT | WMSZ_BOTTOMLEFT
        | WMSZ_BOTTOMRIGHT => {
            let bounded_client_height = match violation {
                AspectViolation::TooTall => client_width * MIN_ASPECT_HEIGHT / MIN_ASPECT_WIDTH,
                AspectViolation::TooWide => {
                    ceil_div(client_width * MAX_ASPECT_HEIGHT, MAX_ASPECT_WIDTH)
                }
            };
            let bounded_outer_height = bounded_client_height + insets.height;
            if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
                rect.top = rect.bottom - bounded_outer_height;
            } else {
                rect.bottom = rect.top + bounded_outer_height;
            }
        }
        _ => return false,
    }
    true
}

fn maximize_geometry_for_work_area(
    monitor: RECT,
    work_area: RECT,
    insets: FrameInsets,
) -> Option<MaximizeGeometry> {
    let work_width = work_area.right - work_area.left;
    let work_height = work_area.bottom - work_area.top;
    let client_width = (work_width - insets.width).max(1);
    let client_height = (work_height - insets.height).max(1);
    let violation = aspect_violation(client_width, client_height)?;
    let monitor_x = work_area.left - monitor.left;
    let monitor_y = work_area.top - monitor.top;

    match violation {
        AspectViolation::TooWide => {
            let width = (client_height * MAX_ASPECT_WIDTH / MAX_ASPECT_HEIGHT + insets.width)
                .min(work_width);
            Some(MaximizeGeometry {
                width,
                height: work_height,
                x: monitor_x + (work_width - width) / 2,
                y: monitor_y,
            })
        }
        AspectViolation::TooTall => {
            let height = (client_width * MIN_ASPECT_HEIGHT / MIN_ASPECT_WIDTH + insets.height)
                .min(work_height);
            Some(MaximizeGeometry {
                width: work_width,
                height,
                x: monitor_x,
                y: monitor_y + (work_height - height) / 2,
            })
        }
    }
}

fn ceil_div(numerator: i32, denominator: i32) -> i32 {
    (numerator + denominator - 1) / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    fn client_size(rect: RECT, insets: FrameInsets) -> (i32, i32) {
        (
            rect.right - rect.left - insets.width,
            rect.bottom - rect.top - insets.height,
        )
    }

    #[test]
    fn in_range_window_resize_is_unchanged() {
        let insets = FrameInsets {
            width: 16,
            height: 40,
        };
        let mut rect = rect(20, 30, 1300, 750);
        let original = rect;

        assert!(!constrain_sizing_rect(&mut rect, WMSZ_RIGHT, insets));
        assert_eq!(rect.left, original.left);
        assert_eq!(rect.top, original.top);
        assert_eq!(rect.right, original.right);
        assert_eq!(rect.bottom, original.bottom);
    }

    #[test]
    fn horizontal_resize_stops_at_the_wide_ratio_limit() {
        let insets = FrameInsets {
            width: 16,
            height: 40,
        };
        let mut rect = rect(100, 50, 4100, 1050);

        assert!(constrain_sizing_rect(&mut rect, WMSZ_RIGHT, insets));
        assert_eq!(rect.left, 100);
        assert_eq!(rect.bottom, 1050);
        let (width, height) = client_size(rect, insets);
        assert!(i64::from(width) * 3 <= i64::from(height) * 7);
    }

    #[test]
    fn left_edge_resize_stops_at_the_tall_ratio_limit() {
        let insets = FrameInsets {
            width: 16,
            height: 40,
        };
        let mut rect = rect(100, 50, 700, 1250);

        assert!(constrain_sizing_rect(&mut rect, WMSZ_LEFT, insets));
        assert_eq!(rect.right, 700);
        assert_eq!(rect.bottom, 1250);
        let (width, height) = client_size(rect, insets);
        assert!(i64::from(width) * 3 >= i64::from(height) * 2);
    }

    #[test]
    fn vertical_resize_stops_at_the_tall_ratio_limit() {
        let insets = FrameInsets {
            width: 16,
            height: 40,
        };
        let mut rect = rect(100, 50, 1116, 2090);

        assert!(constrain_sizing_rect(&mut rect, WMSZ_BOTTOM, insets));
        assert_eq!(rect.left, 100);
        assert_eq!(rect.top, 50);
        let (width, height) = client_size(rect, insets);
        assert!(i64::from(width) * 3 >= i64::from(height) * 2);
    }

    #[test]
    fn corner_resize_stops_at_the_wide_ratio_limit_and_keeps_its_anchor() {
        let insets = FrameInsets {
            width: 16,
            height: 40,
        };
        let mut rect = rect(100, 100, 4100, 1100);

        assert!(constrain_sizing_rect(&mut rect, WMSZ_TOPLEFT, insets));
        assert_eq!((rect.right, rect.bottom), (4100, 1100));
        let (width, height) = client_size(rect, insets);
        assert!(i64::from(width) * 3 <= i64::from(height) * 7);
    }

    #[test]
    fn maximize_is_centered_inside_supported_ratio_on_ultrawide_monitor() {
        let insets = FrameInsets {
            width: 16,
            height: 40,
        };
        let geometry =
            maximize_geometry_for_work_area(rect(0, 0, 3000, 1000), rect(0, 0, 3000, 1000), insets)
                .expect("ultrawide monitor needs a bounded maximize size");

        assert_eq!((geometry.width, geometry.height), (2256, 1000));
        assert_eq!((geometry.x, geometry.y), (372, 0));
    }

    #[test]
    fn maximize_is_centered_inside_supported_ratio_on_portrait_monitor() {
        let insets = FrameInsets {
            width: 16,
            height: 40,
        };
        let geometry =
            maximize_geometry_for_work_area(rect(0, 0, 1000, 1800), rect(0, 0, 1000, 1800), insets)
                .expect("portrait monitor needs a bounded maximize size");

        assert_eq!((geometry.width, geometry.height), (1000, 1516));
        assert_eq!((geometry.x, geometry.y), (0, 142));
    }
}
