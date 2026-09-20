"""生成 assets/icon.ico —— 只用标准库，不依赖 PIL。

图形沿用界面本身的语言：深色圆角底 + 空心圆（任务行左边那个完成按钮）+ 横条（任务文字）。
大尺寸画三行"清单"，小尺寸只留一个圆，避免 16px 糊成一团。

改完图标后重新跑：
    python tools/make_icon.py
然后按 README「换图标」一节重新生成 assets/icon.res。
"""

import os
import struct
import zlib

BG = (30, 30, 34)  # 和 app.rs 里的 BG 一致
FG = (214, 214, 214)  # 和 app.rs 里的 TEXT 一致
SIZES = [16, 20, 24, 32, 48, 64, 128, 256]
SS = 4  # 超采样倍数，用来做抗锯齿


def rounded_rect(x, y, w, h, r):
    """返回一个判定函数：点是否落在圆角矩形内（坐标已归一化到 0..1）。"""

    def inside(px, py):
        if not (x <= px <= x + w and y <= py <= y + h):
            return False
        # 四个角各自按圆判定
        cx = min(max(px, x + r), x + w - r)
        cy = min(max(py, y + r), y + h - r)
        return (px - cx) ** 2 + (py - cy) ** 2 <= r * r

    return inside


def ring(cx, cy, radius, stroke):
    outer, inner = radius, radius - stroke

    def inside(px, py):
        d2 = (px - cx) ** 2 + (py - cy) ** 2
        return inner * inner <= d2 <= outer * outer

    return inside


def capsule(x0, x1, cy, half_h):
    """两端半圆的横条。"""
    return rounded_rect(x0, cy - half_h, x1 - x0, half_h * 2, half_h)


def shapes_for(size):
    """按尺寸选图形：小图只留一个圆，大图画三行清单。"""
    if size <= 32:
        # 32 及以下画三行的话圆环只有几个像素，会糊；只留一个圆更干净。
        return [ring(0.5, 0.5, 0.30, 0.105)]

    out = []
    rows = [(0.275, 0.80), (0.5, 0.66), (0.725, 0.745)]
    for cy, bar_end in rows:
        out.append(ring(0.255, cy, 0.078, 0.030))
        out.append(capsule(0.40, bar_end, cy, 0.038))
    return out


def render(size):
    """渲染成 RGBA 像素列表（自上而下，逐行）。"""
    bg_shape = rounded_rect(0.0, 0.0, 1.0, 1.0, 0.22)
    fg_shapes = shapes_for(size)
    n = size * SS
    px = []
    for y in range(size):
        row = []
        for x in range(size):
            bg_hits = 0
            fg_hits = 0
            for sy in range(SS):
                for sx in range(SS):
                    u = (x * SS + sx + 0.5) / n
                    v = (y * SS + sy + 0.5) / n
                    if bg_shape(u, v):
                        bg_hits += 1
                        if any(s(u, v) for s in fg_shapes):
                            fg_hits += 1
            total = SS * SS
            alpha = bg_hits / total
            if alpha == 0:
                row.append((0, 0, 0, 0))
                continue
            # 前景在背景之上做覆盖率混合
            t = fg_hits / bg_hits
            color = tuple(round(BG[i] + (FG[i] - BG[i]) * t) for i in range(3))
            row.append((color[0], color[1], color[2], round(alpha * 255)))
        px.append(row)
    return px


def to_png(px):
    size = len(px)
    raw = b"".join(
        b"\x00" + b"".join(struct.pack("4B", *p) for p in row) for row in px
    )

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def to_dib(px):
    """经典 BMP 格式的图标条目：BITMAPINFOHEADER + 自下而上的 BGRA + AND 掩码。"""
    size = len(px)
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    xor = b"".join(
        b"".join(struct.pack("4B", p[2], p[1], p[0], p[3]) for p in px[y])
        for y in reversed(range(size))
    )
    mask_row = ((size + 31) // 32) * 4  # 每行按 4 字节对齐
    and_mask = b"\x00" * (mask_row * size)
    return header + xor + and_mask


def build_ico(path):
    entries = []
    for size in SIZES:
        px = render(size)
        # 大图用 PNG 压缩（Vista+ 的惯例），小图用经典 BMP，兼容性最稳。
        blob = to_png(px) if size >= 128 else to_dib(px)
        entries.append((size, blob))

    offset = 6 + 16 * len(entries)
    header = struct.pack("<HHH", 0, 1, len(entries))
    directory = b""
    for size, blob in entries:
        directory += struct.pack(
            "<BBBBHHII",
            size if size < 256 else 0,
            size if size < 256 else 0,
            0,
            0,
            1,
            32,
            len(blob),
            offset,
        )
        offset += len(blob)

    with open(path, "wb") as f:
        f.write(header + directory + b"".join(blob for _, blob in entries))


if __name__ == "__main__":
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    assets = os.path.join(root, "assets")
    os.makedirs(assets, exist_ok=True)
    out = os.path.join(assets, "icon.ico")
    build_ico(out)
    print("wrote", out, os.path.getsize(out), "bytes")

    # 顺手导出一张 256 的 PNG 方便肉眼检查
    preview = os.path.join(assets, "icon-preview.png")
    with open(preview, "wb") as f:
        f.write(to_png(render(256)))
    print("wrote", preview)
