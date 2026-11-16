"""Generate the original Poetrhymer vector icon and Windows raster variants.
Run with Python and Pillow. No fonts or third-party artwork are used.
"""
from pathlib import Path
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1] / "assets" / "icon"
ROOT.mkdir(parents=True, exist_ok=True)
NAVY = "#192230"
GOLD = "#eeb968"
OUTER = [(68, 56), (125, 56), ((162, 56), (182, 78), (182, 112)),
         ((182, 146), (160, 166), (125, 166)), (94, 166), (94, 206), (68, 206)]
INNER = [(94, 82), (94, 140), (124, 140), ((145, 140), (156, 130), (156, 112)),
         ((156, 94), (145, 82), (124, 82))]

def svg_path(segments):
    result = f"M{segments[0][0]} {segments[0][1]}"
    for segment in segments[1:]:
        if len(segment) == 2:
            result += f" L{segment[0]} {segment[1]}"
        else:
            result += " C" + " ".join(f"{x} {y}" for x, y in segment)
    return result + " Z"

def polygon(segments, scale):
    points = [segments[0]]
    for segment in segments[1:]:
        if len(segment) == 2:
            points.append(segment)
        else:
            start = points[-1]
            a, b, end = segment
            for step in range(1, 49):
                t = step / 48
                points.append(tuple((1-t)**3*start[i] + 3*(1-t)**2*t*a[i] + 3*(1-t)*t*t*b[i] + t**3*end[i] for i in range(2)))
    return [(round(x*scale), round(y*scale)) for x, y in points]

path = svg_path(OUTER) + " " + svg_path(INNER)
(ROOT / "poetrhymer.svg").write_text(
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">'
    f'<rect x="8" y="8" width="240" height="240" rx="48" fill="{NAVY}"/>'
    f'<path fill="{GOLD}" fill-rule="evenodd" d="{path}"/></svg>\n', encoding="utf-8")
scale = 4
image = Image.new("RGBA", (256*scale, 256*scale))
draw = ImageDraw.Draw(image)
draw.rounded_rectangle((8*scale, 8*scale, 248*scale, 248*scale), radius=48*scale, fill=NAVY)
draw.polygon(polygon(OUTER, scale), fill=GOLD)
draw.polygon(polygon(INNER, scale), fill=NAVY)
image = image.resize((256, 256), Image.Resampling.LANCZOS)
image.save(ROOT / "poetrhymer.png")
image.save(ROOT / "poetrhymer.ico", sizes=[(n,n) for n in (16,24,32,48,64,128,256)])
print("Generated SVG, PNG and multi-resolution ICO.")
