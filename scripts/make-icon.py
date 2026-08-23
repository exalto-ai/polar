#!/usr/bin/env python3
"""
Draw the app mark: Orbit, cut as a coin.

    python3 scripts/make-icon.py && npx tauri icon ../assets/icon.png --prefix app

The mark is a point in polar coordinates — a ring for the grid, one arm swung
out from the centre to fix a bearing. The document is the centre; the editor,
an agent over MCP and the relay are the same document seen from different
angles.

The coin cut inverts it: the mark is knocked out of a filled disc, so the tile
shows through the ring and the arm. A filled silhouette is what survives a Dock
— outlines at 32px turn to lint, and there is nothing here to thin out.

The tile is Polar Blue and the disc is white, rather than the other way round.
A dark tile put a blue mark on a near-black field and read as a widget; the
brand colour belongs to the whole tile, which is the only part of the icon
visible at a glance in a Dock. The gradient is the accent lifted at the top and
deepened at the bottom, both derived from the one hex, so the icon cannot
disagree with DESIGN.md.

Geometry is authored on the 96x96 field the SVGs in assets/orbit use, and
scaled up, so the two never drift. Masks are drawn at 4x and downsampled,
because PIL has no antialiasing of its own.
"""
from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 1024
FIELD = 96  # the coordinate space assets/orbit/*.svg are drawn in
SS = 4  # supersampling factor for the knockout mask

ACCENT = (47, 111, 237)  # #2f6fed, Polar Blue; see DESIGN.md
MARK = (255, 255, 255)


def _shade(colour: tuple[int, int, int], amount: float) -> tuple[int, int, int]:
    """Lift towards white for positive amounts, towards black for negative."""
    target = 255 if amount > 0 else 0
    return tuple(round(c + (target - c) * abs(amount)) for c in colour)


BACKGROUND_TOP = _shade(ACCENT, 0.10)
BACKGROUND_BOTTOM = _shade(ACCENT, -0.16)

# macOS rounds app icons generously; anything squarer looks foreign in the Dock.
CORNER = 0.225

DISC_R = 34.0  # the coin
RING_R = 21.0  # knocked out of it
RING_W = 4.5
ARM_W = 7.0
ARM_END = (72.0, 24.0)  # 45 degrees up and to the right, crossing the ring
CENTRE = (48.0, 48.0)


def _tile() -> Image.Image:
    """The rounded, vertically graded tile the mark is cut out of."""
    column = Image.new("RGB", (1, SIZE))
    pen = ImageDraw.Draw(column)
    for y in range(SIZE):
        t = y / (SIZE - 1)
        pen.point(
            (0, y),
            fill=tuple(
                int(BACKGROUND_TOP[i] + (BACKGROUND_BOTTOM[i] - BACKGROUND_TOP[i]) * t)
                for i in range(3)
            ),
        )

    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, SIZE - 1, SIZE - 1], radius=int(SIZE * CORNER), fill=255
    )
    image = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    image.paste(column.resize((SIZE, SIZE)), (0, 0), mask)
    return image


def _coin_mask() -> Image.Image:
    """Opaque where the coin is, transparent where the mark is cut out of it."""
    scale = SIZE * SS / FIELD

    def at(x: float, y: float) -> tuple[float, float]:
        return x * scale, y * scale

    def blob(centre: tuple[float, float], radius: float) -> list[float]:
        cx, cy = at(*centre)
        r = radius * scale
        return [cx - r, cy - r, cx + r, cy + r]

    mask = Image.new("L", (SIZE * SS, SIZE * SS), 0)
    draw = ImageDraw.Draw(mask)

    draw.ellipse(blob(CENTRE, DISC_R), fill=255)
    draw.ellipse(blob(CENTRE, RING_R), outline=0, width=int(RING_W * scale))

    # PIL's lines have square ends, so the arm's round cap is drawn as a disc.
    draw.line([at(*CENTRE), at(*ARM_END)], fill=0, width=int(ARM_W * scale))
    draw.ellipse(blob(ARM_END, ARM_W / 2), fill=0)

    return mask.resize((SIZE, SIZE), Image.LANCZOS)


def draw_icon() -> Image.Image:
    image = _tile()
    image.paste(MARK, (0, 0), _coin_mask())
    return image


if __name__ == "__main__":
    target = Path(__file__).resolve().parent.parent / "assets" / "icon.png"
    target.parent.mkdir(exist_ok=True)
    draw_icon().save(target)
    print(f"wrote {target}")
