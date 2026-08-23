#!/usr/bin/env python3
"""
Draw the app mark: a caret resting in a paragraph.

    python3 scripts/make-icon.py && npx tauri icon ../assets/icon.png --prefix app

The product is a cursor that several people and several agents share, so the
icon is the caret the editor already draws — thin bar, round cap — sitting in a
gap in a block of text, rather than a page or a pen.

Everything is sized so it survives being shrunk to 32px. The caret is the only
saturated shape; the text lines are quiet enough to read as texture instead of
competing with it, and the cap has to fit inside the gap between two lines or it
smears into the line above.
"""
from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 1024
BACKGROUND_TOP = (26, 28, 33)
BACKGROUND_BOTTOM = (13, 15, 19)
ACCENT = (86, 148, 255)
TEXT = (128, 137, 150, 255)

# macOS rounds app icons generously; anything squarer looks foreign in the Dock.
CORNER = 0.225


def draw_icon() -> Image.Image:
    image = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    # A vertical gradient, subtle enough to read as one colour when small.
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
    image.paste(column.resize((SIZE, SIZE)), (0, 0), mask)

    draw = ImageDraw.Draw(image)
    left, right = int(SIZE * 0.235), int(SIZE * 0.765)
    line_height, gap = int(SIZE * 0.052), int(SIZE * 0.062)
    radius, top = line_height // 2, int(SIZE * 0.315)
    caret_column = int(SIZE * 0.545)

    # Four lines, the last one short, as text actually falls.
    widths = [right, right, right, int(SIZE * 0.60)]
    caret_row = 1
    caret_y = top

    for row, line_right in enumerate(widths):
        y = top + row * (line_height + gap)
        if row != caret_row:
            draw.rounded_rectangle([left, y, line_right, y + line_height], radius=radius, fill=TEXT)
            continue
        # Break the line so the caret sits between words, not on top of them.
        pad = int(SIZE * 0.030)
        draw.rounded_rectangle([left, y, caret_column - pad, y + line_height], radius=radius, fill=TEXT)
        draw.rounded_rectangle([caret_column + pad, y, line_right, y + line_height], radius=radius, fill=TEXT)
        caret_y = y

    bar_width = int(SIZE * 0.026)
    overhang = int(SIZE * 0.011)
    cap_radius = int(SIZE * 0.024)

    draw.rounded_rectangle(
        [
            caret_column - bar_width // 2,
            caret_y - overhang,
            caret_column + bar_width // 2,
            caret_y + line_height + overhang,
        ],
        radius=bar_width // 2,
        fill=ACCENT,
    )
    cap_centre = caret_y - overhang - int(SIZE * 0.012)
    draw.ellipse(
        [
            caret_column - cap_radius,
            cap_centre - cap_radius,
            caret_column + cap_radius,
            cap_centre + cap_radius,
        ],
        fill=ACCENT,
    )
    return image


if __name__ == "__main__":
    target = Path(__file__).resolve().parent.parent / "assets" / "icon.png"
    target.parent.mkdir(exist_ok=True)
    draw_icon().save(target)
    print(f"wrote {target}")
