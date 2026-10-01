import sys, fitz
class It:
    def __init__(s, x0,y0,x1,y1,w): s.x0=x0; s.x1=x1; s.y=y0; s.str=w; s.size=y1-y0

def detect(items):
    n=len(items)
    if n<12: return None
    minX=min(i.x0 for i in items); maxX=max(i.x1 for i in items)
    width=maxX-minX
    if width<=0: return None
    B=100
    hist=[0]*B
    for i in items:
        k=int((i.x0-minX)/width*(B-1)); k=max(0,min(B-1,k)); hist[k]+=1
    lo,hi=int(B*0.20),int(B*0.80)
    leftMass=sum(hist[int(B*0.05):lo]); rightMass=sum(hist[hi:int(B*0.95)+1])
    if leftMass<n*0.15 or rightMass<n*0.15:
        return dict(ok=False, why=f"mass L={leftMass:.0f} R={rightMass:.0f} n={n}")
    thresh=max(2, int(n*0.012))
    bestStart=-1; bestLen=0; runStart=-1
    for k in range(lo,hi+1):
        if hist[k]<=thresh:
            if runStart<0: runStart=k
            L=k-runStart+1
            if L>bestLen: bestLen=L; bestStart=runStart
        else: runStart=-1
    if bestLen>=2:
        return dict(ok=True, center=minX+(bestStart+bestLen/2)/B*width, gapbins=bestLen)
    return dict(ok=False, why=f"no gap len={bestLen}")

doc=fitz.open(sys.argv[1])
for p in range(doc.page_count):
    words=doc[p].get_text("words")
    items=[It(w[0],w[1],w[2],w[3],w[4]) for w in words if w[4].strip()]
    r=detect(items)
    if r.get('ok'):
        print(f"p{p+1:3d} TWO-COL center={r['center']:6.1f} gapbins={r['gapbins']}")
    else:
        print(f"p{p+1:3d} single  {r.get('why','')}")
