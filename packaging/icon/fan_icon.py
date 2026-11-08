import math, sys
# Usage : python packaging/icon/fan_icon.py assets/icons
from PIL import Image, ImageDraw
# Hélice : 5 pales larges, balayées, bouts arrondis. Coordonnées dans un carré 64x64, centre (32,32).
C, BLADES, R0, R1, SWEEP = 32.0, 5, 5.0, 26.5, math.radians(42)
def blade(k):
    base = 2 * math.pi * k / BLADES
    lead, trail = [], []
    n = 40
    for i in range(n + 1):
        t = i / n
        r = R0 + (R1 - R0) * t
        a = base + SWEEP * t ** 1.3
        w = math.radians(24 + 36 * math.sin(math.pi * t * 0.85))  # largeur angulaire
        lead.append((r, a))
        trail.append((r, a - w))
    # bout arrondi : arc entre bord d'attaque et bord de fuite au rayon max, bombé vers l'extérieur
    r_tip, a1, a2 = lead[-1][0], lead[-1][1], trail[-1][1]
    tip = [(r_tip + 3.0 * math.sin(math.pi * j / 12), a1 + (a2 - a1) * j / 12) for j in range(1, 12)]
    poly = lead + tip + trail[::-1]
    return [(C + r * math.cos(a), C + r * math.sin(a)) for r, a in poly]
blades = [blade(k) for k in range(BLADES)]

def render(size):
    s = 16; S = size * s; f = S / 64
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, S - 1, S - 1], radius=14 * f, fill=(0, 0, 0, 255))
    for b in blades:
        d.polygon([(x * f, y * f) for x, y in b], fill=(255, 255, 255, 255))
    for r, col in ((8.5, (0, 0, 0, 255)), (6.0, (255, 255, 255, 255))):
        d.ellipse([(C - r) * f, (C - r) * f, (C + r) * f, (C + r) * f], fill=col)
    return img.resize((size, size), Image.LANCZOS)

def svg():
    paths = "\n".join(
        '    <path d="M' + " L".join(f"{x:.2f} {y:.2f}" for x, y in b) + ' Z"/>' for b in blades)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <!-- Icône ControlAnything : hélice blanche sur fond noir. Générée par packaging/icon/fan_icon.py ; fan.png et controlanything.ico en sont les rendus. -->
  <rect width="64" height="64" rx="14" fill="#000000"/>
  <g fill="#ffffff">
{paths}
  </g>
  <circle cx="32" cy="32" r="8.5" fill="#000000"/>
  <circle cx="32" cy="32" r="6" fill="#ffffff"/>
</svg>
'''
out = sys.argv[1]
render(256).save("preview.png") if out == "preview" else None
if out != "preview":
    render(64).save(f"{out}/fan.png")
    render(256).save(f"{out}/controlanything.ico", sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
    open(f"{out}/fan.svg", "w", encoding="utf-8", newline="\n").write(svg())
