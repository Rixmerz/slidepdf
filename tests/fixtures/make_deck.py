# Regenerates tests/fixtures/deck.pptx:
#   uv run --with python-pptx --with pillow python tests/fixtures/make_deck.py tests/fixtures

import sys
from pptx import Presentation
from pptx.util import Inches, Pt
from pptx.dml.color import RGBColor
from pptx.enum.shapes import MSO_SHAPE
from PIL import Image, ImageDraw
out = sys.argv[1]
img = Image.new("RGB", (640, 400), (30, 90, 160)); d = ImageDraw.Draw(img)
for i in range(0, 640, 40): d.ellipse((i, 100+i//8, i+80, 180+i//8), fill=(240, 180, 40))
img.save(out + "/pic.jpg", quality=80)
png = Image.new("RGBA", (200, 200), (0,0,0,0)); d = ImageDraw.Draw(png); d.ellipse((10,10,190,190), fill=(200,30,60,200)); png.save(out+"/pic.png")
p = Presentation()
s = p.slides.add_slide(p.slide_layouts[0])
s.shapes.title.text = "Capacitación en Seguridad"
s.placeholders[1].text = "Módulo 1 · Introducción — año 2026"
s = p.slides.add_slide(p.slide_layouts[1])
s.shapes.title.text = "Objetivos del curso"
tf = s.placeholders[1].text_frame
tf.text = "Comprender los riesgos más comunes en el área de trabajo"
for t, lvl in [("Identificar peligros", 1), ("Usar el equipo de protección personal correctamente y siempre", 1), ("Reportar incidentes ¿a quién?", 0)]:
    para = tf.add_paragraph(); para.text = t; para.level = lvl
s = p.slides.add_slide(p.slide_layouts[5])
s.shapes.title.text = "Formas e imágenes"
r = s.shapes.add_shape(MSO_SHAPE.ROUNDED_RECTANGLE, Inches(0.5), Inches(2), Inches(3), Inches(1.5))
r.fill.solid(); r.fill.fore_color.rgb = RGBColor(0x2E, 0x8B, 0x57); r.text = "Rectángulo"
e = s.shapes.add_shape(MSO_SHAPE.OVAL, Inches(4), Inches(2), Inches(1.5), Inches(1.5)); e.line.color.rgb = RGBColor(0,0,0)
s.shapes.add_picture(out + "/pic.jpg", Inches(6), Inches(2), Inches(3.5))
s.shapes.add_picture(out + "/pic.png", Inches(0.5), Inches(4.5), Inches(1.5))
tb = s.shapes.add_textbox(Inches(4), Inches(5), Inches(5), Inches(1)); rr = tb.text_frame.paragraphs[0].add_run(); rr.text = "Texto rojo en negrita"; rr.font.bold = True; rr.font.size = Pt(24); rr.font.color.rgb = RGBColor(200,0,0)
s = p.slides.add_slide(p.slide_layouts[1])
s.shapes.title.text = "Slide con la misma imagen"
s.shapes.add_picture(out + "/pic.jpg", Inches(1), Inches(3), Inches(3))
s.background.fill.solid(); s.background.fill.fore_color.rgb = RGBColor(0xF5, 0xF0, 0xE1)
s.placeholders[1].text = "Griego Ωμέγα, flecha → y check ✓"
p.save(out + "/deck.pptx")
