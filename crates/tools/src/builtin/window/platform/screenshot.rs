use serde_json::Value;

/// Capture the primary screen and save it as a PNG at the host-selected
/// managed path. The pixel buffer is copied
/// out of the GDI device context before it is released, then encoded
/// with the `image` crate — the capture itself never touches the file.
pub(crate) fn capture_screen(path: std::path::PathBuf) -> anyhow::Result<Value> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
        CreateDCW, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HGDIOBJ, ReleaseDC,
        SRCCOPY, SelectObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        if width <= 0 || height <= 0 {
            anyhow::bail!("failed to query screen size ({width}x{height})");
        }

        // Primary-screen DC. `CreateDCW("DISPLAY")` is the classic
        // full-screen DC, but some environments (no interactive desktop
        // access, disconnected RDP session) reject it. `GetDC(NULL)`
        // retrieves the screen DC directly and is more lenient, so fall
        // back to it. The two are released differently (DeleteDC vs
        // ReleaseDC), tracked by `screen_dc_is_getdc`.
        let (screen_dc, screen_dc_is_getdc) = {
            let dc = CreateDCW(
                std::ptr::null(),
                "DISPLAY"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect::<Vec<u16>>()
                    .as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            );
            if !dc.is_null() {
                (dc, false)
            } else {
                let dc = GetDC(std::ptr::null_mut());
                if dc.is_null() {
                    anyhow::bail!("failed to create screen DC (GDI error {})", GetLastError());
                }
                (dc, true)
            }
        };
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_null() {
            if screen_dc_is_getdc {
                ReleaseDC(std::ptr::null_mut(), screen_dc);
            } else {
                DeleteDC(screen_dc);
            }
            anyhow::bail!("failed to create memory DC (GDI error {})", GetLastError());
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        if bitmap.is_null() {
            DeleteDC(mem_dc);
            if screen_dc_is_getdc {
                ReleaseDC(std::ptr::null_mut(), screen_dc);
            } else {
                DeleteDC(screen_dc);
            }
            anyhow::bail!(
                "failed to create compatible bitmap (GDI error {})",
                GetLastError()
            );
        }
        let old_obj = SelectObject(mem_dc, bitmap as HGDIOBJ);
        let ok = BitBlt(mem_dc, 0, 0, width, height, screen_dc, 0, 0, SRCCOPY);

        // Read the pixel data out before releasing the DCs.
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = -height; // top-down rows
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        let copied = GetDIBits(
            mem_dc,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr() as *mut _,
            &mut bmi,
            DIB_RGB_COLORS,
        );

        if !old_obj.is_null() {
            SelectObject(mem_dc, old_obj);
        }
        DeleteObject(bitmap as _);
        DeleteDC(mem_dc);
        if screen_dc_is_getdc {
            ReleaseDC(std::ptr::null_mut(), screen_dc);
        } else {
            DeleteDC(screen_dc);
        }

        if ok == 0 {
            anyhow::bail!("BitBlt failed (GDI error {})", GetLastError());
        }
        if copied == 0 {
            anyhow::bail!("GetDIBits failed (GDI error {})", GetLastError());
        }

        // BGRA (GDI) -> RGBA for the image crate.
        let mut rgba = pixels.clone();
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let img = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
            .ok_or_else(|| anyhow::anyhow!("invalid screenshot buffer"))?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        img.save(&path)?;

        Ok(serde_json::json!({
            "path": path.to_string_lossy().to_string(),
            "width": width,
            "height": height,
            "format": "png",
            "hint": "Open the image with the files tool (read) to view it.",
        }))
    }
}
