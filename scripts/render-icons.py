# -*- coding: utf-8 -*-
"""从 scripts/logo.svg（矢量源）渲染 Tauri 所需的整套图标：PNG + 多尺寸 ICO。

用 PyMuPDF 做 SVG -> 位图光栅化（带 alpha，透明背景），
保证任意尺寸下边缘都清晰（矢量源，无二次下采样模糊）。

运行：python scripts/render-icons.py
"""
import os
import struct

import fitz

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SVG = os.path.join(ROOT, "scripts", "logo.svg")
ICONS = os.path.join(ROOT, "src-tauri", "icons")

# (文件名, 尺寸)
PNG_JOBS = [
    ("32x32.png", 32),
    ("128x128.png", 128),
    ("128x128@2x.png", 256),
    ("icon.png", 512),
]
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]


def render_png(size):
    doc = fitz.open(SVG)
    page = doc[0]
    scale = size / 512.0  # SVG 名义尺寸 512x512
    pix = page.get_pixmap(matrix=fitz.Matrix(scale, scale), alpha=True)
    doc.close()
    if pix.n != 4:
        raise RuntimeError(f"expected RGBA, got n={pix.n}")
    return pix.tobytes("png"), pix.width, pix.height


def write_ico(pngs):
    """把若干 (size, png_bytes) 组装成多尺寸 ICO（PNG 压缩，Vista+ 通用）。"""
    count = len(pngs)
    header = struct.pack("<HHH", 0, 1, count)
    entries = b""
    datas = b""
    offset = 6 + count * 16
    for size, data in pngs:
        wb = 0 if size >= 256 else size
        entries += struct.pack(
            "<BBBBHHII", wb, wb, 0, 0, 1, 32, len(data), offset
        )
        offset += len(data)
        datas += data
    return header + entries + datas


def main():
    os.makedirs(ICONS, exist_ok=True)

    cache = {}
    for name, size in PNG_JOBS:
        data, w, h = render_png(size)
        cache[size] = data
        path = os.path.join(ICONS, name)
        with open(path, "wb") as f:
            f.write(data)
        print(f"wrote {name} ({w}x{h}, {len(data)} bytes)")

    ico_pngs = []
    for size in ICO_SIZES:
        data = cache.get(size) or render_png(size)[0]
        ico_pngs.append((size, data))

    ico_path = os.path.join(ICONS, "icon.ico")
    with open(ico_path, "wb") as f:
        f.write(write_ico(ico_pngs))
    print(f"wrote icon.ico ({len(ico_pngs)} sizes, {len(write_ico(ico_pngs))} bytes)")

    # 同时更新 scripts/logo.png（1024 高清栅格源，供旧 gen-icons.mjs 使用）
    data, _, _ = render_png(1024)
    logo_png = os.path.join(ROOT, "scripts", "logo.png")
    with open(logo_png, "wb") as f:
        f.write(data)
    print(f"wrote scripts/logo.png (1024x1024)")


if __name__ == "__main__":
    main()
