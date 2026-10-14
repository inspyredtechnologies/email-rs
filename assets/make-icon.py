"""Draws the Outlook RS app icon: a blue tile with an envelope and a calendar badge.

Writes assets/icon.ico (16-256 px) and assets/icon.png (256 px). Requires Pillow.
Run: python assets/make-icon.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
SS = 4  # supersampling factor for smooth edges

TOP = (0x1E, 0x8C, 0xE6)
BOTTOM = (0x0A, 0x55, 0xA8)
DARK = (0x0A, 0x4A, 0x94)
WHITE = (255, 255, 255)


def tile(size: int) -> Image.Image:
    """Rounded blue tile with a vertical gradient."""
    s = size * SS
    grad = Image.new("RGB", (1, s))
    for y in range(s):
        t = y / (s - 1)
        grad.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)))
    grad = grad.resize((s, s))
    mask = Image.new("L", (s, s), 0)
    m = round(s * 0.04)
    ImageDraw.Draw(mask).rounded_rectangle((m, m, s - m - 1, s - m - 1), radius=round(s * 0.2), fill=255)
    out = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    out.paste(grad, (0, 0), mask)
    return out


def envelope(d: ImageDraw.ImageDraw, box, line_w: int):
    x0, y0, x1, y1 = box
    r = round((y1 - y0) * 0.1)
    d.rounded_rectangle(box, radius=r, fill=WHITE)
    # The flap: a V from the top corners to the middle.
    mid = ((x0 + x1) / 2, y0 + (y1 - y0) * 0.55)
    d.line([(x0 + line_w, y0 + line_w), mid, (x1 - line_w, y0 + line_w)], fill=DARK, width=line_w, joint="curve")


def calendar_badge(d: ImageDraw.ImageDraw, box, ring: int):
    x0, y0, x1, y1 = box
    w = x1 - x0
    # A ring in the tile color separates the badge from the envelope.
    d.rounded_rectangle((x0 - ring, y0 - ring, x1 + ring, y1 + ring), radius=round(w * 0.22), fill=BOTTOM)
    d.rounded_rectangle(box, radius=round(w * 0.16), fill=WHITE)
    header = y0 + (y1 - y0) * 0.3
    d.rounded_rectangle((x0, y0, x1, header), radius=round(w * 0.16), fill=TOP)
    d.rectangle((x0, header - w * 0.1, x1, header), fill=TOP)
    # 3x2 grid of date cells.
    cell = w * 0.16
    gap_x = (w - 3 * cell) / 4
    gap_y = ((y1 - header) - 2 * cell) / 3
    for row in range(2):
        for col in range(3):
            cx = x0 + gap_x + col * (cell + gap_x)
            cy = header + gap_y + row * (cell + gap_y)
            d.rectangle((cx, cy, cx + cell, cy + cell), fill=DARK)


def draw(size: int) -> Image.Image:
    img = tile(size)
    s = size * SS
    d = ImageDraw.Draw(img)
    if size <= 24:
        # Small sizes: just a big envelope so it stays legible.
        envelope(d, (s * 0.18, s * 0.28, s * 0.82, s * 0.72), max(SS, round(s * 0.07)))
    else:
        envelope(d, (s * 0.14, s * 0.24, s * 0.72, s * 0.62), round(s * 0.045))
        calendar_badge(d, (s * 0.52, s * 0.50, s * 0.86, s * 0.84), round(s * 0.035))
    return img.resize((size, size), Image.LANCZOS)


def main():
    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = {n: draw(n) for n in sizes}
    images[256].save(HERE / "icon.png")
    # Pillow writes each size from the matching frame when given exact-size images.
    images[256].save(HERE / "icon.ico", sizes=[(n, n) for n in sizes], append_images=[images[n] for n in sizes[:-1]])
    print("wrote", HERE / "icon.ico", HERE / "icon.png")


if __name__ == "__main__":
    main()
