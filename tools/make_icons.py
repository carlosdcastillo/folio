"""Generate Folio's cross-platform application icons.

Requires ImageMagick for SVG rasterisation. The native ICO and ICNS containers
are assembled here so generating icons works the same on Linux, Windows, and
macOS.

Run from the repo root:  python tools/make_icons.py
"""

import os
import shutil
import struct
import subprocess


ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(ROOT, "tools", "folio-icon.svg")
SMALL_SOURCE = os.path.join(ROOT, "tools", "folio-icon-small.svg")
OUT = os.path.join(ROOT, "crates", "folio-app", "icons")
WINDOWS_SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)


def render_png(path, size, source=SOURCE):
    subprocess.run(
        [
            "magick",
            "-background",
            "none",
            source,
            "-resize",
            f"{size}x{size}",
            "-depth",
            "8",
            "-define",
            "png:color-type=6",
            "-strip",
            path,
        ],
        check=True,
    )
    with open(path, "rb") as fh:
        return fh.read()


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


def write_icns(path, images):
    """Write modern PNG-backed ICNS entries, including Retina aliases."""
    entries = [
        (b"icp4", images[16]),
        (b"icp5", images[32]),
        (b"ic11", images[32]),
        (b"icp6", images[64]),
        (b"ic12", images[64]),
        (b"ic07", images[128]),
        (b"ic08", images[256]),
        (b"ic13", images[256]),
        (b"ic09", images[512]),
        (b"ic14", images[512]),
        (b"ic10", images[1024]),
    ]
    body = b"".join(kind + struct.pack(">I", len(blob) + 8) + blob for kind, blob in entries)
    with open(path, "wb") as fh:
        fh.write(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main():
    if shutil.which("magick") is None:
        raise SystemExit("ImageMagick 7 is required (the `magick` command was not found)")

    os.makedirs(OUT, exist_ok=True)
    images = {}
    temporary = []
    names = {
        32: "32x32.png",
        64: "64x64.png",
        128: "128x128.png",
        256: "128x128@2x.png",
        1024: "icon.png",
    }
    for size in (16, 32, 64, 96, 128, 256, 512, 1024):
        name = names.get(size, f".{size}x{size}.png")
        path = os.path.join(OUT, name)
        images[size] = render_png(path, size)
        if size not in names:
            temporary.append(path)
        else:
            print("wrote", name)

    windows_images = {}
    for size in WINDOWS_SIZES:
        if size > 64:
            windows_images[size] = images[size]
            continue
        path = os.path.join(OUT, f".{size}x{size}-windows.png")
        windows_images[size] = render_png(path, size, SMALL_SOURCE)
        temporary.append(path)

    write_ico(
        os.path.join(OUT, "icon.ico"),
        [(size, windows_images[size]) for size in WINDOWS_SIZES],
    )
    print("wrote icon.ico")

    write_icns(os.path.join(OUT, "icon.icns"), images)
    print("wrote icon.icns")

    for path in temporary:
        os.remove(path)


if __name__ == "__main__":
    main()
