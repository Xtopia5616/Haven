//! Native Windows startup surface shown while the main WebView2 is created.
//!
//! The HWND is created, painted, and destroyed only on its owning worker
//! thread. Windows supplies valid HWND/HDC/PAINTSTRUCT pointers for each
//! synchronous window callback; the callback never stores those borrowed
//! values. UTF-16 buffers passed to Win32 stay alive for the full call.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetStockObject, InvalidateRect, NULL_PEN, RoundRect, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics, HTCAPTION,
    MB_ICONERROR, MB_OK, MB_RETRYCANCEL, MB_TOPMOST, MSG, MessageBoxW, PM_REMOVE, PeekMessageW,
    PostQuitMessage, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage,
    WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_NCHITTEST, WM_PAINT, WM_QUIT, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const WINDOW_WIDTH: i32 = 360;
const WINDOW_HEIGHT: i32 = 190;
const CAPTION: &[u16] = &[72, 97, 118, 101, 110, 0];
const CLASS_NAME: &[u16] = &[72, 97, 118, 101, 110, 83, 116, 97, 114, 116, 117, 112, 0];
const ID_RETRY: i32 = 4;
const BM_BUILDER_FAILURE: &str = "Haven 无法创建主界面。请检查 WebView2 Runtime 后重新启动 Haven。";

pub(crate) fn show_builder_failure_dialog() {
    let text: Vec<u16> = BM_BUILDER_FAILURE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            CAPTION.as_ptr(),
            MB_ICONERROR | MB_OK | MB_TOPMOST,
        )
    };
}

pub(crate) fn show_failure_dialog(message: &str) -> NativeAction {
    let text: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            CAPTION.as_ptr(),
            MB_ICONERROR | MB_RETRYCANCEL | MB_TOPMOST,
        )
    };
    if result == ID_RETRY {
        NativeAction::Retry
    } else {
        NativeAction::Exit
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NativeAction {
    Retry,
    Exit,
}

enum NativeCommand {
    Loading,
    Ready,
    Failed(&'static str),
    Close,
}

pub(crate) struct NativeStartupSplash {
    commands: Sender<NativeCommand>,
    actions: std::sync::Mutex<Receiver<NativeAction>>,
}

impl NativeStartupSplash {
    pub(crate) fn start() -> Result<Self, String> {
        let (commands_tx, commands_rx) = mpsc::channel();
        let (actions_tx, actions_rx) = mpsc::channel();
        let (created_tx, created_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("haven-native-startup-surface".into())
            .spawn(move || run_native_window(commands_rx, actions_tx, created_tx))
            .map_err(|error| format!("启动原生加载窗口失败: {error}"))?;
        match created_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                let _ = commands_tx.send(NativeCommand::Close);
                return Err(format!("原生加载窗口未能及时创建: {error}"));
            }
        }
        Ok(Self {
            commands: commands_tx,
            actions: std::sync::Mutex::new(actions_rx),
        })
    }

    pub(crate) fn show_loading(&self) {
        let _ = self.commands.send(NativeCommand::Loading);
    }

    pub(crate) fn hide(&self) {
        let _ = self.commands.send(NativeCommand::Ready);
    }

    pub(crate) fn show_failure(&self, message: &'static str) -> bool {
        self.commands.send(NativeCommand::Failed(message)).is_ok()
    }

    pub(crate) fn take_action(&self) -> Option<NativeAction> {
        self.actions.lock().ok()?.try_recv().ok()
    }

    pub(crate) fn close(&self) {
        let _ = self.commands.send(NativeCommand::Close);
    }
}

impl Drop for NativeStartupSplash {
    fn drop(&mut self) {
        self.close();
    }
}

fn run_native_window(
    commands: Receiver<NativeCommand>,
    actions: Sender<NativeAction>,
    created: mpsc::SyncSender<Result<(), String>>,
) {
    let hwnd = match unsafe { create_window() } {
        Ok(hwnd) => hwnd,
        Err(error) => {
            let _ = created.send(Err(error));
            return;
        }
    };
    let _ = created.send(Ok(()));
    let mut message = MSG::default();
    let mut running = true;
    let mut visible = true;
    while running {
        for command in commands.try_iter() {
            match command {
                NativeCommand::Loading => unsafe {
                    visible = true;
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                },
                NativeCommand::Ready => unsafe {
                    visible = false;
                    ShowWindow(hwnd, SW_HIDE);
                },
                NativeCommand::Failed(text) => {
                    visible = true;
                    unsafe {
                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        InvalidateRect(hwnd, std::ptr::null(), 0);
                    }
                    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                    let result = unsafe {
                        MessageBoxW(
                            hwnd,
                            text.as_ptr(),
                            CAPTION.as_ptr(),
                            MB_ICONERROR | MB_RETRYCANCEL | MB_TOPMOST,
                        )
                    };
                    let action = if result == ID_RETRY {
                        NativeAction::Retry
                    } else {
                        NativeAction::Exit
                    };
                    let _ = actions.send(action);
                }
                NativeCommand::Close => running = false,
            }
        }

        if !running {
            break;
        }
        while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_QUIT {
                running = false;
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if running {
            if visible {
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                thread::sleep(Duration::from_millis(24));
            } else {
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
    unsafe { DestroyWindow(hwnd) };
}

unsafe fn create_window() -> Result<HWND, String> {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    if instance.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let window_class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: CLASS_NAME.as_ptr(),
    };
    if unsafe { RegisterClassW(&window_class) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let x = (unsafe { GetSystemMetrics(0) } - WINDOW_WIDTH) / 2;
    let y = (unsafe { GetSystemMetrics(1) } - WINDOW_HEIGHT) / 2;
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS_NAME.as_ptr(),
            CAPTION.as_ptr(),
            WS_POPUP,
            x,
            y,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
    Ok(hwnd)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match message {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            unsafe { paint(hwnd) };
            0
        }
        WM_NCHITTEST => HTCAPTION as isize,
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut paint = unsafe { std::mem::zeroed() };
    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
    if hdc.is_null() {
        return;
    }
    let mut client = RECT::default();
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client) };
    let background = unsafe { CreateSolidBrush(rgb(24, 27, 36)) };
    unsafe { FillRect(hdc, &client, background) };
    unsafe { DeleteObject(background) };

    let elapsed = STARTED
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f32();
    let center_x = (client.right - client.left) / 2;
    let center_y = 72;
    let bar_width = 5;
    let gap = 4;
    let heights = [14, 24, 18];
    for (index, base_height) in heights.into_iter().enumerate() {
        let delay = [-0.16, 0.0, 0.16][index];
        let elapsed_in_cycle = elapsed - delay;
        let (offset_y, opacity) = if elapsed_in_cycle < 0.0 {
            (0.0, 1.0)
        } else {
            let progress = elapsed_in_cycle.rem_euclid(1.2) / 1.2;
            if progress < 0.5 {
                let eased = ease_in_out_sine(progress * 2.0);
                (4.0 - eased * 8.0, 0.5 + eased * 0.5)
            } else {
                let eased = ease_in_out_sine((progress - 0.5) * 2.0);
                (-4.0 + eased * 8.0, 1.0 - eased * 0.5)
            }
        };
        let left = center_x - (bar_width * 3 + gap * 2) / 2 + index as i32 * (bar_width + gap);
        let top = center_y - base_height / 2 + offset_y.round() as i32;
        let color = blend_primary(opacity);
        let brush = unsafe { CreateSolidBrush(color) };
        let previous_brush = unsafe { SelectObject(hdc, brush) };
        let previous_pen = unsafe { SelectObject(hdc, GetStockObject(NULL_PEN)) };
        unsafe {
            RoundRect(
                hdc,
                left,
                top,
                left + bar_width,
                top + base_height,
                bar_width,
                bar_width,
            )
        };
        unsafe {
            SelectObject(hdc, previous_pen);
            SelectObject(hdc, previous_brush);
            DeleteObject(brush);
        }
    }

    unsafe {
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, rgb(205, 210, 222));
    };
    let caption: Vec<u16> = "正在启动 Haven…".encode_utf16().collect();
    let mut caption_rect = RECT {
        left: 0,
        top: 122,
        right: client.right,
        bottom: 156,
    };
    unsafe {
        DrawTextW(
            hdc,
            caption.as_ptr(),
            caption.len() as i32,
            &mut caption_rect,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );
        EndPaint(hwnd, &paint);
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
}

fn ease_in_out_sine(progress: f32) -> f32 {
    0.5 - (std::f32::consts::PI * progress).cos() * 0.5
}

fn blend_primary(opacity: f32) -> u32 {
    let blend_channel = |background: u8, primary: u8| {
        (f32::from(background) + f32::from(primary - background) * opacity).round() as u8
    };
    rgb(
        blend_channel(24, 51),
        blend_channel(27, 120),
        blend_channel(36, 214),
    )
}

static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
