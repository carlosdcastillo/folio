"""Generate Folio's application icons.

No image library on the machine and none wanted: the mark is simple enough to
rasterise directly. Two offset pages — the corpus and its history — on the
Alpaca Assist editor background, in the same accent blue the UI uses.

Run from the repo root:  python tools/make_icons.py
"""

import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "crates", "folio-app", "icons")

BG = (0x1E, 0x1E, 0x1E)          # --bg-primary
PAGE_BACK = (0x26, 0x4F, 0x78)   # --accent-secondary
PAGE_FRONT = (0xCC, 0xCC, 0xCC)  # --text-primary
ACCENT = (0x00, 0x7A, 0xCC)      # --accent-primary


def rounded_rect_alpha(x, y, w, h, radius, px, py, samples=4):
    """Coverage of a rounded rectangle at a pixel, supersampled for smooth edges."""
    hits = 0
    step = 1.0 / samples
    for sy in range(samples):
        for sx in range(samples):
            fx = px + (sx + 0.5) * step
            fy = py + (sy + 0.5) * step
            if not (x <= fx <= x + w and y <= fy <= y + h):
                continue
            # Distance into a corner, if any.
            cx = min(max(fx, x + radius), x + w - radius)
            cy = min(max(fy, y + radius), y + h - radius)
            dx, dy = fx - cx, fy - cy
            if dx * dx + dy * dy <= radius * radius:
                hits += 1
    return hits / float(samples * samples)


def over(dst, src, alpha):
    return tuple(int(round(s * alpha + d * (1 - alpha))) for d, s in zip(dst, src))


def render(size):
    s = size / 256.0
    pixels = [[BG for _ in range(size)] for _ in range(size)]

    shapes = [
        # (x, y, w, h, radius, colour)
        (54, 40, 130, 168, 12, PAGE_BACK),
        (74, 62, 130, 168, 12, PAGE_FRONT),
    ]

    for (x, y, w, h, r, colour) in shapes:
        x, y, w, h, r = x * s, y * s, w * s, h * s, max(1.0, r * s)
        for py in range(size):
            if py + 1 < y or py > y + h:
                continue
            for px in range(size):
                if px + 1 < x or px > x + w:
                    continue
                a = rounded_rect_alpha(x, y, w, h, r, px, py)
                if a > 0:
                    pixels[py][px] = over(pixels[py][px], colour, a)

    # Three accent rules on the front page: a document with lines in it.
    for i, (ly, lw) in enumerate([(96, 86), (124, 86), (152, 54)]):
        x = 94 * s
        y = ly * s
        w = lw * s
        h = 10 * s
        colour = ACCENT if i == 0 else (0x85, 0x85, 0x85)
        for py in range(size):
            if py + 1 < y or py > y + h:
                continue
            for px in range(size):
                if px + 1 < x or px > x + w:
                    continue
                a = rounded_rect_alpha(x, y, w, h, min(h / 2, 5 * s), px, py)
                if a > 0:
                    pixels[py][px] = over(pixels[py][px], colour, a)

    rows = bytearray()
    for py in range(size):
        rows.append(0)  # PNG filter type 0
        for px in range(size):
            r, g, b = pixels[py][px]
            rows += bytes((r, g, b, 255))
    return bytes(rows), pixels


def png_chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def write_png(path, size, raw):
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # 8-bit RGBA
    blob = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", header)
        + png_chunk(b"IDAT", zlib.compress(raw, 9))
        + png_chunk(b"IEND", b"")
    )
    with open(path, "wb") as fh:
        fh.write(blob)
    return blob


def write_ico(path, entries):
    """entries: list of (size, png_bytes). ICO may carry PNG payloads directly."""
    count = len(entries)
    out = struct.pack("<HHH", 0, 1, count)
    offset = 6 + 16 * count
    directory = b""
    payload = b""
    for size, blob in entries:
        directory += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0,
            0,
            1,
            32,
            len(blob),
            offset,
        )
        payload += blob
        offset += len(blob)
    with open(path, "wb") as fh:
        fh.write(out + directory + payload)


def main():
    os.makedirs(OUT, exist_ok=True)
    blobs = {}
    for size in (256, 128, 64, 32):
        raw, _ = render(size)
        name = {256: "icon.png", 128: "128x128.png", 64: "64x64.png", 32: "32x32.png"}[size]
        blobs[size] = write_png(os.path.join(OUT, name), size, raw)
        print("wrote", name)

    write_ico(
        os.path.join(OUT, "icon.ico"),
        [(32, blobs[32]), (64, blobs[64]), (128, blobs[128]), (256, blobs[256])],
    )
    print("wrote icon.ico")


if __name__ == "__main__":
    main()
