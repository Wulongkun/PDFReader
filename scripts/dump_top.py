import sys, fitz, io
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')
doc=fitz.open(sys.argv[1])
page=doc[0]
words=page.get_text("words")
# print words with y > 600 sorted by y desc, grouped into lines
from collections import defaultdict
lines=defaultdict(list)
for w in words:
    if w[1] > 600:
        lines[round(w[1])].append(w)
for y in sorted(lines.keys(), reverse=True):
    ws=sorted(lines[y], key=lambda w:w[0])
    x0=min(w[0] for w in ws); x1=max(w[2] for w in ws)
    text=" ".join(w[4] for w in ws)
    kind = "FULL" if (x0 < 280 and x1 > 330) else ("LEFT" if x1 <= 310 else "RIGHT")
    print(f"y={y:4d} x=[{x0:5.1f},{x1:5.1f}] {kind:5s} {text[:60]}")
