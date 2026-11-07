# Benchmark deck generator; see scripts/bench.sh.
import sys
from pptx import Presentation
from pptx.util import Inches
from PIL import Image, ImageDraw
out=sys.argv[1]
im=Image.new("RGBA",(8000,8000),(0,0,0,0)); d=ImageDraw.Draw(im)
for k in range(0,8000,400): d.ellipse((k,k,k+3000,k+3000),fill=(k%255,90,200,160))
im.save(out+"/huge.png")
p=Presentation(); s=p.slides.add_slide(p.slide_layouts[6]); s.shapes.add_picture(out+"/huge.png",Inches(1),Inches(1),Inches(5)); p.save(out+"/huge.pptx")
