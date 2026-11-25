"""Generates the app icon in every accent colour.

    python tools/accent_icons.py

Writes assets/accent/<rrggbb>.ico (Windows: window, taskbar, shortcuts) and
assets/accent/<rrggbb>.png (Linux menu icon). The gradient
math matches `accent_stops` in src/ui/mod.rs, so the in-app logo looks the same.
"""
import colorsys
import os
from PIL import Image, ImageDraw

ACCENTS = [0xFF0033, 0xF0307E, 0xAB47BC, 0x3EA6FF, 0x00BFA5, 0x2BA640, 0xFFA000]
BRAND = 0xF0307E
OUT = os.path.join(os.path.dirname(__file__), "..", "assets", "accent")
S = 1024  # drawn large, scaled down for smooth edges


def rgb(c):
    return ((c >> 16) & 255, (c >> 8) & 255, c & 255)


def hsl_shift(c, dh, sk, lk):
    r, g, b = (x / 255 for x in rgb(c))
    h, l, s = colorsys.rgb_to_hls(r, g, b)
    h = (h + dh / 360.0) % 1.0
    s = max(0.0, min(1.0, s * sk))
    l = max(0.0, min(1.0, l * lk))
    r, g, b = colorsys.hls_to_rgb(h, l, s)
    return (round(r * 255), round(g * 255), round(b * 255))


def stops(accent):
    if accent == BRAND:
        return [rgb(0xFF5A5F), rgb(0xF0307E), rgb(0x7A3CF5)]
    return [hsl_shift(accent, -12, 1.0, 1.18), rgb(accent), hsl_shift(accent, 32, 0.95, 0.72)]


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def gradient(st):
    # diagonal gradient, top-left to bottom-right, through three stops
    img = Image.new("RGB", (S, S))
    px = img.load()
    for y in range(S):
        for x in range(S):
            t = (x + y) / (2 * (S - 1))
            px[x, y] = lerp(st[0], st[1], t * 2) if t < 0.5 else lerp(st[1], st[2], (t - 0.5) * 2)
    return img


def logo(accent):
    k = S / 64
    base = gradient(stops(accent))
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([2 * k, 2 * k, 62 * k, 62 * k], radius=17 * k, fill=255)
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(base, (0, 0), mask)
    # gloss on the upper half
    gloss = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    gd = ImageDraw.Draw(gloss)
    for y in range(int(2 * k), int(32 * k)):
        a = int(255 * 0.28 * (1 - (y - 2 * k) / (30 * k)))
        gd.line([(0, y), (S, y)], fill=(255, 255, 255, a))
    gmask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(gmask).rounded_rectangle([2 * k, 2 * k, 62 * k, 62 * k], radius=17 * k, fill=255)
    out.alpha_composite(Image.composite(gloss, Image.new("RGBA", (S, S), (0, 0, 0, 0)), gmask))
    d = ImageDraw.Draw(out)
    w = int(3.6 * k)
    for (x1, x2, y, op) in [(11, 21, 24, 0.65), (8, 22, 32, 0.9), (11, 21, 40, 0.65)]:
        layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        ld = ImageDraw.Draw(layer)
        ld.line([(x1 * k, y * k), (x2 * k, y * k)], fill=(255, 255, 255, int(255 * op)), width=w)
        r = w / 2
        for cx in (x1 * k, x2 * k):
            ld.ellipse([cx - r, y * k - r, cx + r, y * k + r], fill=(255, 255, 255, int(255 * op)))
        out.alpha_composite(layer)
    tri = [(29 * k, 19.5 * k), (48.5 * k, 32 * k), (29 * k, 44.5 * k)]
    d.polygon(tri, fill=(255, 255, 255, 255))
    d.line(tri + [tri[0]], fill=(255, 255, 255, 255), width=int(5 * k), joint="curve")
    for (x, y) in tri:
        r = 2.5 * k
        d.ellipse([x - r, y - r, x + r, y + r], fill=(255, 255, 255, 255))
    return out


def main():
    os.makedirs(OUT, exist_ok=True)
    for a in ACCENTS:
        img = logo(a)
        name = f"{a:06x}"
        img.resize((256, 256), Image.LANCZOS).save(os.path.join(OUT, name + ".png"))
        img.resize((256, 256), Image.LANCZOS).save(
            os.path.join(OUT, name + ".ico"), sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48), (64, 64), (128, 128), (256, 256)]
        )
        print("wrote", name)


if __name__ == "__main__":
    main()
