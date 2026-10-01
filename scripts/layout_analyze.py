import sys, fitz

class It:
    def __init__(s, x0,y0,x1,y1,w):
        s.x0=x0; s.x1=x1; s.y=y0; s.str=w; s.size=y1-y0

def load_items(path):
    doc = fitz.open(path)
    return doc

def detect(items):
    n=len(items)
    if n<12: return None
    minX=min(i.x0 for i in items); maxX=max(i.x1 for i in items)
    width=maxX-minX
    if width<=0: return None
    B=100
    cov=[0]*B
    for i in items:
        a=int((i.x0-minX)/width*(B-1)); b=int((i.x1-minX)/width*(B-1))
        for k in range(max(0,a), min(B-1,b)+1): cov[k]+=1
    W=5; sm=[0]*B
    for k in range(B):
        s=c=0
        for j in range(max(0,k-W//2), min(B,k+W//2+1)): s+=cov[j]; c+=1
        sm[k]=s/c
    lo,hi=int(B*0.25),int(B*0.75)
    lp=max(sm[int(B*0.10):int(B*0.30)])
    rp=max(sm[int(B*0.70):int(B*0.90)])
    seg=sm[lo:hi+1]
    minVal=min(seg); minIdx=lo+seg.index(minVal)
    ratio=minVal/max(lp,rp) if max(lp,rp)>0 else 1
    center=minX+(minIdx+0.5)/B*width
    return dict(center=center, ratio=ratio, minVal=minVal, peak=max(lp,rp), lp=lp, rp=rp)

doc=load_items(sys.argv[1])
print(f"pages={doc.page_count}")
for p in range(doc.page_count):
    page=doc[p]
    words=page.get_text("words")
    items=[It(w[0],w[1],w[2],w[3],w[4]) for w in words if w[4].strip()]
    r=detect(items)
    tag = "TWO-COL" if (r and r['ratio']<0.5) else "single "
    if r:
        print(f"p{p+1:3d} n={len(items):4d} {tag} center={r['center']:6.1f} ratio={r['ratio']:.2f} min={r['minVal']:.0f} peak={r['peak']:.0f} lp={r['lp']:.0f} rp={r['rp']:.0f}")
    else:
        print(f"p{p+1:3d} n={len(items):4d} {tag} (no split)")
