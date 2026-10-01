import sys, fitz
doc=fitz.open(sys.argv[1])
page=doc[0]
words=page.get_text("words")
# group words into lines by y (round to 1pt)
from collections import defaultdict
lines=defaultdict(list)
for w in words:
    y=round(w[1])
    lines[y].append(w)
for y in sorted(lines.keys(), reverse=True):
    ws=sorted(lines[y], key=lambda w:w[0])
    x0=min(w[0] for w in ws); x1=max(w[2] for w in ws)
    text=" ".join(w[4] for w in ws)
    full = x0 < 280 and x1 > 330
    print(f"y={y:4d} x=[{x0:5.1f},{x1:5.1f}] {'FULL' if full else '     '} {text[:70]}")
