# -*- coding: utf-8 -*-
"""把白底 logo 处理成透明底，并渲染整套应用图标（PNG + 多尺寸 ICO）。

输入：D:/副业/code/loho-new4.png（白底 RGB，蓝色圆角方形 + 白色书页符号）。
处理：flood-fill 去掉"白边"（与图像边缘连通的白底），保留被蓝色包围的
      白色符号；对白底边缘做去白（un-premultiply）得到平滑半透明边缘。
输出：src-tauri/icons/ 下的 PNG + icon.ico（256×256 排最前，任务栏图标需要）。
"""
import os
import struct

import numpy as np
from PIL import Image
from scipy import ndimage

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = r"D:/副业/code/loho-new4.png"
ICONS = os.path.join(ROOT, "src-tauri", "icons")
MASTER = os.path.join(ROOT, "scripts", "logo-transparent.png")

PNG_JOBS = [
    ("32x32.png", 32),
    ("128x128.png", 128),
    ("128x128@2x.png", 256),
    ("icon.png", 512),
]
ICO_SIZES = [256, 128, 64, 48, 32, 24, 16]  # 256 排最前，任务栏只取第一个条目


def dewhite(rgb: np.ndarray, threshold: int = 200) -> np.ndarray:
    """去掉与边缘连通的白底，返回 RGBA (H, W, 4) uint8。"""
    h, w, _ = rgb.shape
    whiteness = rgb.min(axis=2).astype(np.int32)  # 越白越大，255=纯白
    mask = whiteness >= threshold
    s = np.ones((3, 3), dtype=np.uint8)
    lab, n = ndimage.label(mask, structure=s)
    border = set()
    border.update(lab[0, :].tolist(), lab[-1, :].tolist(),
                  lab[:, 0].tolist(), lab[:, -1].tolist())
    border.discard(0)
    bg = np.isin(lab, list(border))  # 与边缘连通的白底区域

    alpha = np.full((h, w), 255, dtype=np.float32)
    out_rgb = rgb.astype(np.float32)

    wv = whiteness[bg]
    # 去白：alpha = 255 - min_channel；前景色 = (c - min) * 255 / (255 - min)
    a = np.clip(255 - wv, 0, 255).astype(np.float32)
    # 接近纯白（253~255 的"白边"）直接归零，彻底透明，避免残留 1~2 的不可见灰边
    a[wv >= 248] = 0
    safe = np.maximum(255 - wv, 1).astype(np.float32)
    for c in range(3):
        ch = out_rgb[:, :, c][bg].astype(np.float32)
        fg = np.clip((ch - wv.astype(np.float32)) * 255.0 / safe, 0, 255)
        out_rgb[:, :, c][bg] = fg
    alpha[bg] = a

    out = np.dstack([out_rgb, alpha]).astype(np.uint8)
    return out


def crop_square(rgba: np.ndarray, fill_ratio: float = 0.95) -> np.ndarray:
    """裁掉多余透明边，让 logo 占画布约 fill_ratio（越小留白越少）。"""
    h, w = rgba.shape[:2]
    alpha = rgba[..., 3]
    ys, xs = np.where(alpha > 8)
    x0, y0, x1, y1 = int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())
    cw, ch = x1 - x0 + 1, y1 - y0 + 1
    side = max(cw, ch)
    s = int(round(side / fill_ratio))
    s -= s % 2  # 偶数尺寸
    cx = (x0 + x1) / 2.0
    cy = (y0 + y1) / 2.0
    left = int(round(cx - s / 2.0))
    top = int(round(cy - s / 2.0))
    left = min(max(left, 0), w - s)
    top = min(max(top, 0), h - s)
    return rgba[top:top + s, left:left + s]


def render_png(master: Image.Image, size: int) -> bytes:
    im = master.resize((size, size), Image.LANCZOS)
    im = im.convert("RGBA")
    import io
    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue()


def write_ico(pngs):
    """把若干 (size, png_bytes) 组装成多尺寸 ICO（PNG 压缩，Vista+ 通用）。"""
    count = len(pngs)
    header = struct.pack("<HHH", 0, 1, count)
    entries = b""
    datas = b""
    offset = 6 + count * 16
    for size, data in pngs:
        wb = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", wb, wb, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        datas += data
    return header + entries + datas


def main():
    rgb = np.asarray(Image.open(SRC).convert("RGB"))
    rgba = dewhite(rgb)
    rgba = crop_square(rgba, fill_ratio=0.95)
    master = Image.fromarray(rgba, "RGBA")
    master.save(MASTER)
    print(f"wrote {MASTER} ({master.size[0]}x{master.size[1]})")

    os.makedirs(ICONS, exist_ok=True)
    cache = {}
    for name, size in PNG_JOBS:
        data = render_png(master, size)
        cache[size] = data
        with open(os.path.join(ICONS, name), "wb") as f:
            f.write(data)
        print(f"wrote {name} ({size}x{size}, {len(data)} bytes)")

    ico_pngs = [(size, cache.get(size) or render_png(master, size)) for size in ICO_SIZES]
    ico_path = os.path.join(ICONS, "icon.ico")
    with open(ico_path, "wb") as f:
        f.write(write_ico(ico_pngs))
    print(f"wrote icon.ico ({len(ico_pngs)} sizes, 256 first)")


if __name__ == "__main__":
    main()
