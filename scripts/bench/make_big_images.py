# Benchmark deck generator; see scripts/bench.sh.
import sys
from pptx import Presentation
from pptx.util import Inches
from PIL import Image
out = sys.argv[1]
img = Image.effect_noise((4000, 3000), 60).convert("RGB")
img = Image.merge("RGB", [img.getchannel(0), Image.linear_gradient("L").resize((4000,3000)), img.getchannel(2)])
img.save(out + "/big.jpg", quality=90)
png = Image.new("RGBA", (3000, 3000), (0, 0, 0, 0))
from PIL import ImageDraw
d = ImageDraw.Draw(png); d.ellipse((100, 100, 2900, 2900), fill=(20, 140, 90, 180)); d.rectangle((1200,0,1800,3000), fill=(250,200,0,255))
png.save(out + "/big.png")
p = Presentation()
for crop in (False, True):
    s = p.slides.add_slide(p.slide_layouts[5]); s.shapes.title.text = "Big images" + (" cropped" if crop else "")
    a = s.shapes.add_picture(out + "/big.jpg", Inches(0.5), Inches(2), Inches(3))
    b = s.shapes.add_picture(out + "/big.png", Inches(5), Inches(2), Inches(3))
    if crop:
        a.crop_left = 0.25; a.crop_right = 0.25; b.crop_top = 0.5
p.save(out + "/big.pptx")
