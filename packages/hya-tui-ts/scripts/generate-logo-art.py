#!/usr/bin/env python3
"""Generate quadrant-block terminal art data from the hya project logo.

Reads the project logo PNG (transparent-background sticker), isolates the main
"Hya" letterforms (drops decorations, the underline swoosh, and the tagline
text strip, which the TUI renders as real text), and emits TypeScript modules
where every terminal cell packs a 2x2 sub-pixel bitmap into one Unicode
quadrant/half-block glyph — double the effective resolution of a half-block
render at the same cell count, and a single solid color so each row renders
as one text run.

Usage:
  uv run --with pillow --with numpy --with scipy \\
    packages/hya-tui-ts/scripts/generate-logo-art.py
"""

from __future__ import annotations

import argparse
import math
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_SOURCE = REPO_ROOT / "imgs" / "Hya icon v7.png"

# Fraction of the content bounding box that holds the tagline text strip at
# the bottom of the sticker. The TUI prints the tagline itself, so the art
# stops above it.
TAGLINE_STRIP = 0.145

ALPHA_THRESHOLD = 128

# Color distance from the dominant letter blue that still counts as letter
# body (covers anti-aliased edges blended toward the white sticker border).
LETTER_DISTANCE = 90

# Keep the three letterforms (H, y, a); smaller blue fragments such as the
# "!" flourish and specks are decorations and get dropped.
LETTER_COMPONENTS = 3
LETTER_MIN_FRACTION = 0.10

# 2x2 sub-pixel bitmap per cell, bits TL=1, TR=2, BL=4, BR=8 — one glyph for
# every binary pattern.
GLYPHS = [" ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█"]


def load_artwork(source: Path) -> Image.Image:
    image = Image.open(source).convert("RGBA")
    alpha = image.getchannel("A").point(lambda v: 255 if v > 16 else 0)
    bbox = alpha.getbbox()
    if bbox is None:
        raise SystemExit(f"no visible content in {source}")
    cropped = image.crop(bbox)
    keep = round(cropped.height * (1 - TAGLINE_STRIP))
    return cropped.crop((0, 0, cropped.width, keep))


def dominant_color(artwork: Image.Image) -> tuple[int, int, int]:
    """The most common fully-opaque saturated color — the flat blue of the
    letters (the white sticker body is larger but unsaturated)."""
    arr = np.array(artwork)
    opaque = arr[arr[..., 3] >= 200][:, :3].astype(np.int32)
    spread = opaque.max(axis=1) - opaque.min(axis=1)
    saturated = opaque[spread >= 60]
    pool = saturated if saturated.size else opaque
    colors, counts = np.unique(pool, axis=0, return_counts=True)
    top = colors[counts.argmax()]
    return (int(top[0]), int(top[1]), int(top[2]))


def extract_letters(artwork: Image.Image) -> tuple[Image.Image, str]:
    """Isolate the H/y/a letterforms; return (alpha silhouette, solid hex)."""
    arr = np.array(artwork).astype(np.int32)
    ref = np.array(dominant_color(artwork), dtype=np.int32)
    dist = np.sqrt(((arr[..., :3] - ref) ** 2).sum(axis=2))
    mask = (arr[..., 3] >= 64) & (dist <= LETTER_DISTANCE)

    labels, count = ndimage.label(mask, structure=np.ones((3, 3)))
    if count == 0:
        raise SystemExit("no letter pixels found")
    sizes = np.bincount(labels.ravel())[1:]
    largest = (np.argsort(sizes)[::-1][:LETTER_COMPONENTS] + 1).tolist()
    keep = [label for label in largest if sizes[label - 1] >= sizes.max() * LETTER_MIN_FRACTION]
    letters = np.isin(labels, keep)

    ys, xs = np.nonzero(letters)
    bbox = (int(xs.min()), int(ys.min()), int(xs.max()) + 1, int(ys.max()) + 1)
    alpha = Image.fromarray((letters * 255).astype(np.uint8), "L").crop(bbox)
    hex_color = f"#{int(ref[0]):02x}{int(ref[1]):02x}{int(ref[2]):02x}"
    print(f"letter color: {hex_color}, components kept: {len(keep)}")
    return alpha, hex_color


def cols_for_cells(alpha: Image.Image, cells: int) -> int:
    """Column count whose rendered area (cols x cell rows) is ~`cells`."""
    aspect = alpha.width / alpha.height
    return max(8, round(math.sqrt(cells * 2 * aspect)))


def rasterize(alpha: Image.Image, cols: int) -> list[str]:
    """Downsample the letter silhouette's alpha (area average = exact
    coverage), then encode each cell as a 2x2 sub-pixel quadrant glyph."""
    px_w = cols * 2
    px_h = max(2, round(cols * alpha.height / alpha.width))
    px_h += px_h % 2  # even number of pixel rows so cells pair cleanly
    coverage = alpha.resize((px_w, px_h), Image.BOX)
    bitmap = np.array(coverage) >= ALPHA_THRESHOLD
    rows: list[str] = []
    for y in range(0, px_h, 2):
        chars: list[str] = []
        for x in range(0, px_w, 2):
            pattern = (
                int(bitmap[y, x])
                | int(bitmap[y, x + 1]) << 1
                | int(bitmap[y + 1, x]) << 2
                | int(bitmap[y + 1, x + 1]) << 3
            )
            chars.append(GLYPHS[pattern])
        rows.append("".join(chars))
    return rows


def trim(rows: list[str]) -> list[str]:
    while rows and not rows[0].strip():
        rows.pop(0)
    while rows and not rows[-1].strip():
        rows.pop()
    if not rows:
        return rows
    left = min(len(row) - len(row.lstrip()) for row in rows if row.strip())
    right = max(len(row.rstrip()) for row in rows)
    return [row[left:right].ljust(right - left) for row in rows]


def emit_ts(rows: list[str], hex_color: str, export: str, path: Path) -> None:
    cols = max((len(row) for row in rows), default=0)
    body = ",\n".join(f'    "{row}"' for row in rows)
    lines = [
        "// Generated by scripts/generate-logo-art.py from imgs/Hya icon v7.png — do not edit by hand.",
        "// Each character is a 2x2 sub-pixel quadrant/half-block glyph; space = transparent.",
        "export type LogoArt = { color: string; rows: string[] }",
        f"export const {export}: LogoArt = {{",
        f'  color: "{hex_color}",',
        "  rows: [",
        body + ",",
        "  ],",
        "}",
        "",
    ]
    path.write_text("\n".join(lines))
    print(f"wrote {path} ({cols} cols x {len(rows)} rows = {cols * len(rows)} cells)")


def emit_preview(rows: list[str], hex_color: str, path: Path, scale: int = 16) -> None:
    cols = max((len(row) for row in rows), default=0)
    ink = tuple(int(hex_color[i : i + 2], 16) for i in (1, 3, 5)) + (255,)
    image = Image.new("RGBA", (cols * 2, len(rows) * 2), (24, 24, 28, 255))
    for y, row in enumerate(rows):
        for x, char in enumerate(pad_row(row, cols)):
            pattern = GLYPHS.index(char)
            for dy in (0, 1):
                for dx in (0, 1):
                    if pattern & (1 << (dy * 2 + dx)):
                        image.putpixel((x * 2 + dx, y * 2 + dy), ink)
    image = image.resize((image.width * scale, image.height * scale), Image.NEAREST)
    path.parent.mkdir(parents=True, exist_ok=True)
    image.save(path)
    print(f"wrote preview {path}")


def pad_row(row: str, cols: int) -> str:
    return row.ljust(cols)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--home-cells", type=int, default=184, help="target home art area in cells (previous version: 736)")
    parser.add_argument("--epilogue-cells", type=int, default=82, help="target epilogue art area in cells (previous version: 330)")
    parser.add_argument(
        "--home-out",
        type=Path,
        default=REPO_ROOT / "packages/hya-tui-ts/src/upstream/component/logo-art.data.ts",
    )
    parser.add_argument(
        "--epilogue-out",
        type=Path,
        default=REPO_ROOT / "packages/hya-tui-ts/src/upstream/util/epilogue-art.data.ts",
    )
    parser.add_argument("--preview-dir", type=Path, default=REPO_ROOT / "target/tmp/logo-art")
    args = parser.parse_args()

    artwork = load_artwork(args.source)
    alpha, hex_color = extract_letters(artwork)
    print(f"artwork: {artwork.width}x{artwork.height} -> letters: {alpha.width}x{alpha.height}")

    home = trim(rasterize(alpha, cols_for_cells(alpha, args.home_cells)))
    emit_ts(home, hex_color, "LOGO_ART", args.home_out)
    emit_preview(home, hex_color, args.preview_dir / "home.png")

    epilogue = trim(rasterize(alpha, cols_for_cells(alpha, args.epilogue_cells)))
    emit_ts(epilogue, hex_color, "EPILOGUE_ART", args.epilogue_out)
    emit_preview(epilogue, hex_color, args.preview_dir / "epilogue.png")


if __name__ == "__main__":
    main()
