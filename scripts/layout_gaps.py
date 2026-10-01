import sys, fitz
class It:
    def __init__(s, x0,y0,x1,y1,w): s.x0=x0; s.x1=x1; s.y=y0; s.str=w

def all_gaps(items, pg):
    n=len(items)
    minX=min(i.x0 for i in items); maxX=max(i.x1 for i in items)
    width=maxX-minX
    B=120
    edge=[0]*B; x0h=[0]*B; x1h=[0]*B
    for i in items:
        ka=int((i.x0-minX)/width*(B-1)); ka=max(0,min(B-1,ka)); x0h[ka]+=1
        kb=int((i.x1-minX)/width*(B-1)); kb=max(0,min(B-1,kb)); x1h[kb]+=1
    for k in range(B): edge[k]=x0h[k]+x1h[k]
    lo,hi=int(B*0.15),int(B*0.85)
    thresh=max(2, int(n*0.01))
    gaps=[]; runStart=-1
    for k in range(lo,hi+1):
        if edge[k]<=thresh:
            if runStart<0: runStart=k
        else:
            if runStart>=0:
                L=k-runStart
                if L>=2: gaps.append((runStart, L))
                runStart=-1
    if runStart>=0:
        L=(hi+1)-runStart
        if L>=2: gaps.append((runStart,L))
    centerPx = minX+width/2
    out=[]
    for gs,L in gaps:
        c=minX+(gs+L/2)/B*width
        lc=sum(edge[:gs]); rc=sum(edge[gs+L:])
        out.append((abs(c-centerPx), c, L, lc, rc))
    out.sort()
    return minX, maxX, centerPx, out

doc=fitz.open(sys.argv[1])
for p in range(doc.page_count):
    words=doc[p].get_text("words")
    items=[It(w[0],w[1],w[2],w[3],w[4]) for w in words if w[4].strip()]
    minX,maxX,cpx,gaps=all_gaps(items,p)
    print(f"page {p+1}: center={cpx:.0f}  gaps(closest-to-center first):")
    for d,c,L,lc,rc in gaps[:5]:
        print(f"    center={c:6.1f} dist={d:6.1f} len={L:2d} Lmass={lc:4d} Rmass={rc:4d}")
