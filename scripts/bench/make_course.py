# Benchmark deck generator; see scripts/bench.sh.
import sys
from pptx import Presentation
from pptx.util import Inches
from PIL import Image, ImageDraw
out = sys.argv[1]
p = Presentation()
for i in range(60):
    if i % 2 == 0:
        s = p.slides.add_slide(p.slide_layouts[1]); s.shapes.title.text = f"Tema {i+1}: seguridad en el trabajo"
        tf = s.placeholders[1].text_frame; tf.text = "Uso correcto del equipo de protección personal en faena"
        for t in ["Identificar riesgos antes de comenzar", "Reportar incidentes al supervisor", "Revisar el estado de las herramientas"]:
            tf.add_paragraph().text = t
    else:
        s = p.slides.add_slide(p.slide_layouts[5]); s.shapes.title.text = f"Foto {i+1}"
        path = f"{out}/photo{i}.jpg"
        im = Image.effect_noise((3000, 2000), 40 + i).convert("RGB"); im = Image.merge("RGB", [im.getchannel(0), Image.linear_gradient("L").resize((3000, 2000)).rotate(i), im.getchannel(0)])
        im.save(path, quality=90)
        s.shapes.add_picture(path, Inches(1), Inches(1.8), Inches(8))
        if i % 6 == 1:
            png = Image.new("RGBA", (2400, 1600), (255, 255, 255, 0)); d = ImageDraw.Draw(png)
            for k in range(0, 2400, 120): d.rectangle((k, 200, k + 60, 1400), fill=(20, 60 + k % 180, 200, 220))
            png.save(f"{out}/shot{i}.png"); s.shapes.add_picture(f"{out}/shot{i}.png", Inches(6), Inches(4.5), Inches(3))
p.save(out + "/course.pptx")
