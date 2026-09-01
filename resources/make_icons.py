#!/usr/bin/env python3
"""The app icon, generated.

One drawing — three panels on a workspace, the focused one with its inverted
header, the left column two cells joined — rendered for every target:

  resources/icon_{32,64,128,256,512,1024}.png
      makepad's dock icon. `platform/build.rs` bakes the PNGs it finds next
      to `target/` (or the ones `MAKEPAD_APP_ICON_*` name — see
      `.cargo/config.toml`) into the binary, and `cargo-makepad` desktop and
      android builds read the same files.
  resources/icon.icns    the bundle icon (`cargo-makepad desktop build`).
  resources/icon.ico     the windows executable icon.
  resources/android/res/ the launcher icon: an adaptive icon (one vector
      drawable on a white background, doubling as the monochrome layer for
      themed icons) for API 26+, mipmap PNGs for older launchers.
      `cargo-makepad android` copies the tree into the APK's `res/`.

    python3 resources/make_icons.py [--preview DIR]

Needs Pillow, and `iconutil` for the icns. Small sizes are laid out on the
pixel grid rather than downsampled, so a 16 px icon is three crisp
rectangles, not a blur. `--preview` also writes a contact sheet of every
size on a light and a dark dock, with the android vector rasterized the
way a launcher would show it.
"""

from __future__ import annotations

import math
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent
ANDROID_RES = ROOT / "android" / "res"

INK = (20, 20, 20, 255)  # theme::INK, 0.078
PAPER = (255, 255, 255, 255)  # theme::BG
INK_HEX = "#141414"

# The drawing, as fractions of its own side.
GAP = 0.07  # between panels (the app's 8 pt gap)
HEAD = 0.12  # header height (the app's header is a fixed 26 pt)
STROKE = 0.03  # border weight

# Apple's icon grid: an 824 pt rounded square centred on a 1024 pt canvas,
# corners of radius 185.4 pt with continuous curvature.
MAC_BODY = 824 / 1024
MAC_RADIUS = 185.4 / 824
MAC_SMOOTHING = 0.6
# The drawing's side, as a fraction of the body it sits in.
DRAW_IN_BODY = 0.70
# A hairline around the white body, as a fraction of the canvas: the app's
# 1 pt border, and what keeps the icon a shape on a white desktop.
OUTLINE = 0.006

# Android: a 108 dp adaptive canvas, the launcher masks the inner 72 dp and
# guarantees a 66 dp circle; a 46 dp square fits inside that circle.
ADAPTIVE_CANVAS = 108
ADAPTIVE_WINDOW = 72
ADAPTIVE_DRAW = 46
# Legacy launcher icons: 48 dp, the shape on 44 dp of it.
LEGACY_BODY = 44 / 48


# --- the drawing ------------------------------------------------------------


def layout(side: int) -> list[tuple[int, int, int, int, bool]]:
    """The three panels inside a `side` px square, as half-open integer rects
    `(x0, y0, x1, y1, focused)`: a 2×2 grid whose left column is joined."""
    g = max(1, round(GAP * side))
    col = (side - g) // 2
    r = side - col
    return [
        (0, 0, col, side, True),
        (r, 0, side, col, False),
        (r, r, side, side, False),
    ]


def draw_panels(im: Image.Image, ox: int, oy: int, side: int, detail: bool) -> None:
    """Draws the panels with their top-left corner at `(ox, oy)`, on the
    pixel grid. `detail` adds the title in the focused header."""
    d = ImageDraw.Draw(im)
    w = max(1, round(STROKE * side))
    h = max(w + 1, round(HEAD * side))
    for x0, y0, x1, y1, focused in layout(side):
        x0, x1, y0, y1 = x0 + ox, x1 + ox, y0 + oy, y1 + oy
        d.rectangle([x0, y0, x1 - 1, y1 - 1], fill=PAPER, outline=INK, width=w)
        if focused:
            d.rectangle([x0, y0, x1 - 1, y0 + h - 1], fill=INK)
            if detail:
                t = max(1, round(h * 0.16))
                inset = round(h * 0.32)
                tw = round((x1 - x0) * 0.42)
                ty = y0 + (h - t) // 2
                d.rectangle([x0 + inset, ty, x0 + inset + tw - 1, ty + t - 1], fill=PAPER)
        elif h >= 3 * w:
            # An unfocused header carries a rule; below ~32 px there is no
            # header to speak of, so it stays a plain box.
            d.rectangle([x0, y0 + h - w, x1 - 1, y0 + h - 1], fill=INK)


# --- the shape --------------------------------------------------------------


def squircle(cx: float, cy: float, side: float, radius: float, smoothing: float):
    """Polygon points (clockwise) of a rounded square with continuous-
    curvature corners — Figma's corner-smoothing construction: each corner is
    a circular arc of radius `radius` flanked by two cubic Béziers, the
    whole corner spanning `radius * (1 + smoothing)` along each edge. With
    Apple's radius and 60 % smoothing this is the iOS/macOS icon shape."""
    half = side / 2
    r = radius
    s = smoothing
    p = min((1 + s) * r, half)
    arc_deg = 90 * (1 - s)
    arc_len = math.sin(math.radians(arc_deg / 2)) * r * math.sqrt(2)
    alpha = (90 - arc_deg) / 2
    p34 = r * math.tan(math.radians(alpha / 2))
    beta = 45 * s
    c = p34 * math.cos(math.radians(beta))
    d = c * math.tan(math.radians(beta))
    b = (p - arc_len - c - d) / 3
    a = 2 * b

    def bezier(p0, p1, p2, p3, n=24):
        for i in range(n):
            t = i / n
            u = 1 - t
            yield (
                u * u * u * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t * t * t * p3[0],
                u * u * u * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t * t * t * p3[1],
            )

    # The top-right corner, in a frame whose origin is where the curve
    # leaves the top edge (x right, y down); the arc's centre is where a
    # plain rounded corner's would be.
    local = []
    q3 = (a + b + c, d)
    local += bezier((0, 0), (a, 0), (a + b, 0), q3)
    centre = (p - r, r)
    t0 = math.atan2(q3[1] - centre[1], q3[0] - centre[0])
    t1 = -math.pi / 2 - t0  # mirrored across the corner's diagonal
    n = 24
    for i in range(n):
        t = t0 + (t1 - t0) * i / n
        local.append((centre[0] + r * math.cos(t), centre[1] + r * math.sin(t)))
    q0 = (centre[0] + r * math.cos(t1), centre[1] + r * math.sin(t1))
    local += bezier(q0, (q0[0] + d, q0[1] + c), (q0[0] + d, q0[1] + b + c), (p, p))

    # Place it, then rotate the corner around the centre for the other three.
    pts = [(half - p + x, -half + y) for x, y in local]
    out = []
    for _ in range(4):
        out += [(cx + x, cy + y) for x, y in pts]
        pts = [(-y, x) for x, y in pts]
    return out


def body(size: int, side: float, outline: float) -> Image.Image:
    """A white squircle of the given side centred on a transparent `size`
    canvas, with `outline` px of ink around it (0 for none). Anti-aliased by
    8× supersampling and a box filter, so a sub-pixel outline fades to a
    grey hairline instead of dropping out."""
    f = 8
    big = size * f
    fringe = (INK if outline else PAPER)[:3] + (0,)
    im = Image.new("RGBA", (big, big), fringe)
    d = ImageDraw.Draw(im)
    c = big / 2
    s = side * f
    if outline:
        d.polygon(squircle(c, c, s, s * MAC_RADIUS, MAC_SMOOTHING), fill=INK)
        s -= 2 * outline * f
        d.polygon(squircle(c, c, s, s * MAC_RADIUS, MAC_SMOOTHING), fill=PAPER)
    else:
        d.polygon(squircle(c, c, s, s * MAC_RADIUS, MAC_SMOOTHING), fill=PAPER)
    return im.resize((size, size), Image.Resampling.BOX)


def mac_icon(size: int) -> Image.Image:
    b = size * MAC_BODY
    im = body(size, b, size * OUTLINE)
    s = round(b * DRAW_IN_BODY)
    o = round((size - s) / 2)
    draw_panels(im, o, o, s, detail=size >= 128)
    return im


def legacy_icon(size: int) -> Image.Image:
    b = size * LEGACY_BODY
    im = body(size, b, max(1.0, size * OUTLINE))
    s = round(b * DRAW_IN_BODY)
    o = round((size - s) / 2)
    draw_panels(im, o, o, s, detail=size >= 128)
    return im


# --- android vector ---------------------------------------------------------


def adaptive_foreground_xml() -> str:
    """The drawing as a VectorDrawable on the 108 dp adaptive canvas. Borders
    are stroked paths; the focused header is an even-odd path with the title
    cut out of it, so the same drawable serves as the monochrome layer."""
    side = float(ADAPTIVE_DRAW)
    o = (ADAPTIVE_CANVAS - side) / 2
    w = STROKE * side
    h = HEAD * side
    g = GAP * side
    col = (side - g) / 2
    r = side - col
    panels = [
        (0.0, 0.0, col, side, True),
        (r, 0.0, side, col, False),
        (r, r, side, side, False),
    ]

    def rect(x0, y0, x1, y1):
        return f"M{x0:.2f},{y0:.2f} H{x1:.2f} V{y1:.2f} H{x0:.2f} Z"

    paths = []
    for x0, y0, x1, y1, focused in panels:
        x0, x1, y0, y1 = x0 + o, x1 + o, y0 + o, y1 + o
        i = w / 2  # a stroke is centred on its path: inset so the outer edge is the rect
        paths.append(
            f'    <path\n        android:strokeColor="{INK_HEX}"\n'
            f'        android:strokeWidth="{w:.2f}"\n'
            f'        android:pathData="{rect(x0 + i, y0 + i, x1 - i, y1 - i)}"/>'
        )
        if focused:
            t = h * 0.16
            inset = h * 0.32
            tw = (x1 - x0) * 0.42
            ty = y0 + (h - t) / 2
            paths.append(
                f'    <path\n        android:fillColor="{INK_HEX}"\n'
                f'        android:fillType="evenOdd"\n'
                f'        android:pathData="{rect(x0, y0, x1, y0 + h)} '
                f'{rect(x0 + inset, ty, x0 + inset + tw, ty + t)}"/>'
            )
        else:
            paths.append(
                f'    <path\n        android:fillColor="{INK_HEX}"\n'
                f'        android:pathData="{rect(x0, y0 + h - w, x1, y0 + h)}"/>'
            )
    body_xml = "\n".join(paths)
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<!-- generated by resources/make_icons.py -->\n"
        '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
        f'    android:width="{ADAPTIVE_CANVAS}dp"\n'
        f'    android:height="{ADAPTIVE_CANVAS}dp"\n'
        f'    android:viewportWidth="{ADAPTIVE_CANVAS}"\n'
        f'    android:viewportHeight="{ADAPTIVE_CANVAS}">\n'
        f"{body_xml}\n"
        "</vector>\n"
    )


ADAPTIVE_ICON_XML = """<?xml version="1.0" encoding="utf-8"?>
<!-- generated by resources/make_icons.py -->
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/ic_launcher_background"/>
    <foreground android:drawable="@drawable/ic_launcher_foreground"/>
    <monochrome android:drawable="@drawable/ic_launcher_foreground"/>
</adaptive-icon>
"""

BACKGROUND_XML = """<?xml version="1.0" encoding="utf-8"?>
<!-- generated by resources/make_icons.py -->
<resources>
    <color name="ic_launcher_background">#FFFFFF</color>
</resources>
"""

# dp → px per density bucket.
DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}


# --- preview ----------------------------------------------------------------


def rasterize_vector(xml: str, px_per_dp: int) -> Image.Image:
    """Draws the generated VectorDrawable — the rect-only subset it uses —
    so the preview shows what the phone will, not what the PNG path does."""
    size = ADAPTIVE_CANVAS * px_per_dp
    im = Image.new("RGBA", (size, size), PAPER)
    d = ImageDraw.Draw(im)
    for attrs in re.findall(r"<path\s+(.*?)/>", xml, re.S):
        a = dict(re.findall(r'android:(\w+)="([^"]*)"', attrs))
        rects = re.findall(r"M([\d.]+),([\d.]+) H([\d.]+) V([\d.]+) H[\d.]+ Z", a["pathData"])
        rects = [tuple(float(v) * px_per_dp for v in r) for r in rects]
        if "strokeWidth" in a:
            w = float(a["strokeWidth"]) * px_per_dp
            x0, y0, x1, y1 = rects[0]
            d.rectangle([x0 - w / 2, y0 - w / 2, x1 + w / 2 - 1, y1 + w / 2 - 1], outline=INK, width=round(w))
        else:
            for i, (x0, y0, x1, y1) in enumerate(rects):
                d.rectangle([x0, y0, x1 - 1, y1 - 1], fill=PAPER if i % 2 else INK)
    return im


def launcher_view(im: Image.Image, round_mask: bool) -> Image.Image:
    """What a launcher makes of the adaptive canvas: the 72 dp window, masked
    to a circle (pixel) or a squircle (one ui)."""
    size = im.width
    f = 8
    mask = Image.new("L", (size * f, size * f), 0)
    d = ImageDraw.Draw(mask)
    win = size * f * ADAPTIVE_WINDOW / ADAPTIVE_CANVAS
    c = size * f / 2
    if round_mask:
        d.ellipse([c - win / 2, c - win / 2, c + win / 2, c + win / 2], fill=255)
    else:
        d.polygon(squircle(c, c, win, win * MAC_RADIUS, MAC_SMOOTHING), fill=255)
    im = im.copy()
    im.putalpha(mask.resize((size, size), Image.Resampling.BOX))
    w = round(size * ADAPTIVE_WINDOW / ADAPTIVE_CANVAS)
    o = (size - w) // 2
    return im.crop((o, o, o + w, o + w))


def preview(out: Path, macs: dict[int, Image.Image]) -> None:
    sizes = [16, 32, 64, 128, 256]
    pad = 24
    row_h = 256 + pad
    width = sum(s + pad for s in sizes) + pad + 3 * (192 + pad)
    sheet = Image.new("RGBA", (width, 2 * row_h + pad), (0, 0, 0, 0))
    d = ImageDraw.Draw(sheet)
    d.rectangle([0, 0, width, row_h + pad // 2], fill=(230, 230, 230, 255))
    d.rectangle([0, row_h + pad // 2, width, 2 * row_h + pad], fill=(30, 30, 30, 255))
    vector = rasterize_vector(adaptive_foreground_xml(), 4)
    for row in range(2):
        y = pad + row * row_h
        x = pad
        for s in sizes:
            sheet.alpha_composite(macs[s], (x, y + 256 - s))
            x += s + pad
        x += pad
        for im in (
            legacy_icon(192),
            launcher_view(vector, round_mask=True).resize((192, 192), Image.Resampling.BOX),
            launcher_view(vector, round_mask=False).resize((192, 192), Image.Resampling.BOX),
        ):
            sheet.alpha_composite(im, (x, y + 256 - 192))
            x += 192 + pad
    sheet.save(out / "preview.png")
    macs[1024].save(out / "mac_1024.png")


# --- main -------------------------------------------------------------------


def main(argv: list[str]) -> int:
    preview_dir = None
    if len(argv) >= 2 and argv[1] == "--preview":
        preview_dir = Path(argv[2]) if len(argv) > 2 else Path(tempfile.mkdtemp())

    macs = {s: mac_icon(s) for s in (16, 32, 48, 64, 128, 256, 512, 1024)}
    for s in (32, 64, 128, 256, 512, 1024):
        macs[s].save(ROOT / f"icon_{s}.png", optimize=True)

    macs[256].save(
        ROOT / "icon.ico",
        format="ICO",
        sizes=[(s, s) for s in (16, 32, 48, 64, 128, 256)],
        append_images=[macs[s] for s in (16, 32, 48, 64, 128)],
    )

    if shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as tmp:
            iconset = Path(tmp) / "icon.iconset"
            iconset.mkdir()
            for pt in (16, 32, 128, 256, 512):
                macs[pt].save(iconset / f"icon_{pt}x{pt}.png")
                macs[pt * 2].save(iconset / f"icon_{pt}x{pt}@2x.png")
            subprocess.run(
                ["iconutil", "-c", "icns", "-o", str(ROOT / "icon.icns"), str(iconset)],
                check=True,
            )
    else:
        print("iconutil not found: icon.icns not regenerated", file=sys.stderr)

    for bucket, scale in DENSITIES.items():
        d = ANDROID_RES / f"mipmap-{bucket}"
        d.mkdir(parents=True, exist_ok=True)
        legacy_icon(round(48 * scale)).save(d / "ic_launcher.png", optimize=True)
    (ANDROID_RES / "mipmap-anydpi-v26").mkdir(parents=True, exist_ok=True)
    (ANDROID_RES / "mipmap-anydpi-v26" / "ic_launcher.xml").write_text(ADAPTIVE_ICON_XML)
    (ANDROID_RES / "drawable").mkdir(parents=True, exist_ok=True)
    (ANDROID_RES / "drawable" / "ic_launcher_foreground.xml").write_text(adaptive_foreground_xml())
    (ANDROID_RES / "values").mkdir(parents=True, exist_ok=True)
    (ANDROID_RES / "values" / "ic_launcher_background.xml").write_text(BACKGROUND_XML)

    if preview_dir is not None:
        preview_dir.mkdir(parents=True, exist_ok=True)
        preview(preview_dir, macs)
        print(preview_dir / "preview.png")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
