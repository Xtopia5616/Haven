#!/usr/bin/env python3
"""Generate Haven's raster icon family from the brand geometry.

The SVG in assets/branding/haven-mark.svg is the reviewable source of truth.
This small deterministic renderer mirrors its deliberately simple geometry so
the repository can regenerate PNG, ICO, and ICNS assets without a GUI tool.
"""

from __future__ import annotations

import io
import struct
from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parents[1]
ASSET_DIR = ROOT / "assets" / "branding"
TAURI_DIR = ROOT / "crates" / "app-binary" / "icons"
UI_DIR = ROOT / "ui" / "static"

BLUE = "#2C5090"
BUBBLE = "#D7E3FF"


def scaled(value: float, scale: float) -> int:
    return round(value * scale)


def draw_mark(size: int) -> Image.Image:
    """Render the SVG's flat geometry with supersampling for small sizes."""

    supersample = 4
    canvas = size * supersample
    pixel_scale = supersample * size / 64
    image = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)

    def box(values: tuple[float, float, float, float]) -> tuple[int, int, int, int]:
        return tuple(scaled(value, pixel_scale) for value in values)  # type: ignore[return-value]

    # SVG speech bubble path, represented as the same rounded body plus tail.
    draw.rounded_rectangle(box((8, 15, 56, 46)), radius=scaled(10, pixel_scale), fill=BUBBLE)
    draw.polygon(
        [
            (scaled(21, pixel_scale), scaled(43, pixel_scale)),
            (scaled(21, pixel_scale), scaled(54, pixel_scale)),
            (scaled(32, pixel_scale), scaled(46, pixel_scale)),
        ],
        fill=BUBBLE,
    )

    for bar in ((24, 24.5, 29, 36.5), (29.5, 21.5, 34.5, 39.5), (35, 24.5, 40, 36.5)):
        draw.rounded_rectangle(box(bar), radius=scaled(2.5, pixel_scale), fill=BLUE)

    return image.resize((size, size), Image.Resampling.LANCZOS)


def save_png(size: int, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    draw_mark(size).save(path, format="PNG", optimize=False)


def write_icns(path: Path, image: Image.Image) -> None:
    # Modern macOS accepts PNG-backed ICNS entries. Include the common icon
    # sizes and a 1024px retina entry while keeping one generated geometry.
    entry_types = ((16, "icp4"), (32, "icp5"), (48, "icp6"), (128, "ic07"),
                   (256, "ic08"), (512, "ic09"), (1024, "ic10"))
    entries: list[bytes] = []
    for size, entry_type in entry_types:
        payload = io.BytesIO()
        image.resize((size, size), Image.Resampling.LANCZOS).save(payload, format="PNG", optimize=False)
        data = payload.getvalue()
        entries.append(entry_type.encode("ascii") + struct.pack(">I", len(data) + 8) + data)
    body = b"".join(entries)
    path.write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main() -> None:
    source = ASSET_DIR / "haven-mark.svg"
    if not source.is_file():
        raise SystemExit(f"missing authoritative SVG: {source}")

    # Keep every static product surface on the same source geometry.
    save_png(512, ROOT / "icon.png")
    save_png(512, TAURI_DIR / "icon.png")
    for size in (32, 64, 128, 256):
        name = "128x128@2x.png" if size == 256 else f"{size}x{size}.png"
        save_png(size, TAURI_DIR / name)
    for size in (30, 44, 71, 89, 107, 142, 150, 284, 310):
        save_png(size, TAURI_DIR / f"Square{size}x{size}Logo.png")
    save_png(50, TAURI_DIR / "StoreLogo.png")
    save_png(32, UI_DIR / "favicon.png")

    source_image = draw_mark(1024)
    source_image.save(TAURI_DIR / "icon.ico", format="ICO", sizes=[
        (16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)
    ])
    write_icns(TAURI_DIR / "icon.icns", source_image)


if __name__ == "__main__":
    main()
