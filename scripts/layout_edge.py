import sys, fitz
class It:
    def __init__(s, x0,y0,x1,y1,w): s.x0=x0; s.x1=x1; s.y=y0; s.str=w

def detect(items):
    n=len(items)
    if n<12: return None
    minX=min(i.x0 for i in items); maxX=max(i.x1 for i in items)
    width=maxX-minX
    if width<=0: return None
    B=120
    edge=[0]*B   # word START + word END counts
    x0h=[0]*B; x1h=[0]*B
    for i in items:
        ka=int((i.x0-minX)/width*(B-1)); ka=max(0,min(B-1,ka)); x0h[ka]+=1
        kb=int((i.x1-minX)/width*(B-1)); kb=max(0,min(B-1,kb)); x1h[kb]+=1
    for k in range(B): edge[k]=x0h[k]+x1h[k]
    lo,hi=int(B*0.15),int(B*0.85)
    thresh=max(2, int(n*0.01))
    # 找 central 区域里最长的低值连续段
    bestStart=-1; bestLen=0; runStart=-1
    for k in range(lo,hi+1):
        if edge[k]<=thresh:
            if runStart<0: runStart=k
            L=k-runStart+1
            if L>bestLen: bestLen=L; bestStart=runStart
        else: runStart=-1
    if bestLen>=2:
        center_bin=bestStart+bestLen/2
        leftCount=sum(x0h[:bestStart])+sum(x1h[:bestStart])
        rightCount=sum(x0h[bestStart+bestLen:])+sum(x1h[bestStart+bestLen:])
        if leftCount>=n*0.3 and rightCount>=n*0.3:
            return dict(ok=True, center=minX+center_bin/B*width, gapbins=bestLen)
        return dict(ok=False, why=f"mass L={leftCount} R={rightCount} n={n}")
    return dict(ok=False, why=f"gap len={bestLen}")

doc=fitz.open(sys.argv[1])
for p in range(doc.page_count):
    words=doc[p].get_text("words")
    items=[It(w[0],w[1],w[2],w[3],w[4]) for w in words if w[4].strip()]
    r=detect(items)
    if r and r.get('ok'):
        print(f"p{p+1:3d} TWO-COL center={r['center']:6.1f} gapbins={r['gapbins']}")
    else:
        print(f"p{p+1:3d} single  {r.get('why','') if r else 'n/a'}")
