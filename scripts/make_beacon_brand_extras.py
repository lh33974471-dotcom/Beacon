
from PIL import Image, ImageDraw
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "hoptodesk-main", "res")

# ---- wordmark + flutter assets + svg ----
from PIL import ImageFont

FLUTTER_ASSETS = os.path.join(ROOT, "hoptodesk-main", "flutter", "assets")

def wordmark(base_icon):
    W, H = 1200, 240
    im = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    icon = base_icon.resize((200, 200), Image.LANCZOS)
    im.paste(icon, (10, 20), icon)
    d = ImageDraw.Draw(im)
    try:
        font = ImageFont.truetype(r"C:\Windows\Fonts\segoeuib.ttf", 120)
    except Exception:
        font = ImageFont.load_default()
    d.text((240, 52), "Beacon", font=font, fill=(20, 40, 74, 255))
    im.save(os.path.join(FLUTTER_ASSETS, "logo.png"))
    base_icon.resize((256, 256), Image.LANCZOS).save(os.path.join(FLUTTER_ASSETS, "icon.png"))
    print("wordmark + flutter assets done")

SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#182c54"/>
      <stop offset="1" stop-color="#0d1830"/>
    </linearGradient>
    <clipPath id="tower">
      <polygon points="425,384 599,384 656,819 368,819"/>
    </clipPath>
  </defs>
  <rect width="1024" height="1024" rx="225" fill="url(#bg)"/>
  <polygon points="544,302 922,184 922,420" fill="#ffd166" opacity="0.55"/>
  <polygon points="480,302 102,184 102,420" fill="#ffd166" opacity="0.55"/>
  <polygon points="425,384 599,384 656,819 368,819" fill="#f5f7fa"/>
  <g clip-path="url(#tower)">
    <rect x="368" y="514" width="288" height="70" fill="#e4484d"/>
    <rect x="368" y="654" width="288" height="70" fill="#e4484d"/>
  </g>
  <rect x="392" y="353" width="241" height="36" rx="14" fill="#101a2e"/>
  <rect x="417" y="261" width="189" height="87" rx="12" fill="#101a2e"/>
  <circle cx="512" cy="302" r="53" fill="#ffd166"/>
  <polygon points="403,261 621,261 512,200" fill="#e4484d"/>
  <rect x="328" y="819" width="369" height="56" rx="25" fill="#101a2e"/>
</svg>
"""

def write_svgs():
    for p in (os.path.join(RES, "logo.svg"), os.path.join(FLUTTER_ASSETS, "logo.svg")):
        with open(p, "w", encoding="utf-8") as f:
            f.write(SVG)
    print("svgs done")

_img = Image.open(os.path.join(RES, "icon.png")).convert("RGBA")
wordmark(_img)
write_svgs()

