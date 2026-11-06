from PIL import Image, ImageDraw
import os

S = 4096          # supersample canvas
OUT = 1024
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "hoptodesk-main", "res")

NAVY_TOP = (24, 44, 84)
NAVY_BOT = (13, 24, 48)
RED = (228, 72, 77)
WHITE = (245, 247, 250)
DARK = (16, 26, 46)
YELLOW = (255, 209, 102)

def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))

def rounded_mask(size, radius):
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return m

def tower_half_width(y, y_top, y_bot, w_top, w_bot):
    t = (y - y_top) / (y_bot - y_top)
    return w_top / 2 + (w_bot - w_top) / 2 * t

def main():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    # background: vertical gradient navy, clipped to rounded square
    bg = Image.new("RGBA", (S, S))
    bd = ImageDraw.Draw(bg)
    for y in range(S):
        bd.line([(0, y), (S, y)], fill=lerp(NAVY_TOP, NAVY_BOT, y / S) + (255,))
    mask = rounded_mask(S, int(S * 0.22))
    img.paste(bg, (0, 0), mask)

    cx = S / 2
    # lamp room geometry
    lamp_cy = S * 0.295
    lamp_r = S * 0.052

    # light beams (behind tower)
    beams = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    bdr = ImageDraw.Draw(beams)
    spread = S * 0.115
    reach = S * 0.40
    for sign in (-1, 1):
        bdr.polygon([
            (cx + sign * lamp_r * 0.6, lamp_cy),
            (cx + sign * reach, lamp_cy - spread),
            (cx + sign * reach, lamp_cy + spread),
        ], fill=YELLOW + (150,))
    beams = beams.resize((S // 2, S // 2), Image.LANCZOS).resize((S, S), Image.LANCZOS)  # soften
    img.alpha_composite(beams)

    d = ImageDraw.Draw(img)
    # tower: trapezoid
    y_top, y_bot = S * 0.375, S * 0.80
    w_top, w_bot = S * 0.17, S * 0.28
    d.polygon([
        (cx - w_top / 2, y_top), (cx + w_top / 2, y_top),
        (cx + w_bot / 2, y_bot), (cx - w_bot / 2, y_bot),
    ], fill=WHITE + (255,))
    # red bands clipped to tower silhouette
    bands = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    bd2 = ImageDraw.Draw(bands)
    tower_mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(tower_mask).polygon([
        (cx - w_top / 2, y_top), (cx + w_top / 2, y_top),
        (cx + w_bot / 2, y_bot), (cx - w_bot / 2, y_bot),
    ], fill=255)
    for frac in (0.30, 0.62):
        by = y_top + (y_bot - y_top) * frac
        bh = (y_bot - y_top) * 0.16
        bd2.rectangle([0, by, S, by + bh], fill=RED + (255,))
    img.paste(bands, (0, 0), Image.composite(bands.split()[3], Image.new("L", (S, S), 0), tower_mask))
    d = ImageDraw.Draw(img)
    # gallery (railing platform under lamp room)
    gw, gh = S * 0.235, S * 0.035
    gy = S * 0.345
    d.rounded_rectangle([cx - gw / 2, gy, cx + gw / 2, gy + gh], radius=gh * 0.4, fill=DARK + (255,))
    # lamp room
    lw, lh = S * 0.185, S * 0.085
    ly = S * 0.255
    d.rounded_rectangle([cx - lw / 2, ly, cx + lw / 2, ly + lh], radius=S * 0.012, fill=DARK + (255,))
    # lamp glow
    d.ellipse([cx - lamp_r, lamp_cy - lamp_r, cx + lamp_r, lamp_cy + lamp_r], fill=YELLOW + (255,))
    # roof
    roof_w = lw * 1.15
    d.polygon([(cx - roof_w / 2, ly), (cx + roof_w / 2, ly), (cx, ly - S * 0.06)], fill=RED + (255,))
    # base platform
    bw, bh = S * 0.36, S * 0.055
    by2 = S * 0.80
    d.rounded_rectangle([cx - bw / 2, by2, cx + bw / 2, by2 + bh], radius=bh * 0.45, fill=DARK + (255,))
    # re-clip everything to rounded square
    img.putalpha(Image.composite(img.split()[3], Image.new("L", (S, S), 0), mask))

    base = img.resize((OUT, OUT), Image.LANCZOS)
    os.makedirs(RES, exist_ok=True)
    base.save(os.path.join(RES, "icon.png"))
    base.resize((128, 128), Image.LANCZOS).save(os.path.join(RES, "128x128.png"))
    base.resize((256, 256), Image.LANCZOS).save(os.path.join(RES, "128x128@2x.png"))
    base.resize((32, 32), Image.LANCZOS).save(os.path.join(RES, "32x32.png"))
    ico_sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    base.save(os.path.join(RES, "icon.ico"), sizes=ico_sizes)
    base.save(os.path.join(RES, "tray-icon.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48)])
    print("done:", [f for f in os.listdir(RES) if f.endswith((".ico", ".png"))])

main()


