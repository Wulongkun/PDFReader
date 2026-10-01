import sys, fitz, io, statistics
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')
def median(a):
    if not a: return 0
    s=sorted(a); m=len(s)//2
    return s[m] if len(s)%2 else (s[m-1]+s[m])/2
def extract(items_raw):
    return [dict(str=w[4],x0=w[0],x1=w[2],y=w[1],size=w[3]-w[1]) for w in items_raw if w[4].strip()]
def detect_split(items):
    n=len(items)
    minX=min(i['x0'] for i in items); maxX=max(i['x1'] for i in items)
    width=maxX-minX; B=120; edge=[0]*B
    for i in items:
        a=max(0,min(B-1,int((i['x0']-minX)/width*(B-1)))); edge[a]+=1
        b=max(0,min(B-1,int((i['x1']-minX)/width*(B-1)))); edge[b]+=1
    lo,hi=int(B*0.15),int(B*0.85); thresh=max(2,n*0.01); gaps=[]; run=-1
    for k in range(lo,hi+1):
        if edge[k]<=thresh:
            if run<0: run=k
        else:
            if run>=0:
                if k-run>=2: gaps.append([run,k-run])
                run=-1
    if run>=0 and hi+1-run>=2: gaps.append([run,hi+1-run])
    if not gaps: return None
    cpx=minX+width/2; best=None
    for gs,L in gaps:
        c=minX+(gs+L/2)/B*width; left=sum(edge[:gs]); right=sum(edge[gs+L:])
        if left<n*0.3 or right<n*0.3: continue
        d=abs(c-cpx)
        if best is None or d<best[0]: best=(d,c)
    return best[1] if best else None
def build_rows(items):
    items=sorted(items,key=lambda i:(i['y'],i['x0'])); ytol=median([i['size'] for i in items])*0.5
    rows=[]
    for it in items:
        if rows and abs(rows[-1]['y']-it['y'])<=ytol:
            rows[-1]['items'].append(it); rows[-1]['y']=(rows[-1]['y']*(len(rows[-1]['items'])-1)+it['y'])/len(rows[-1]['items'])
        else: rows.append(dict(y=it['y'],items=[it]))
    for r in rows: r['items'].sort(key=lambda i:i['x0'])
    return rows
def row_to_line(items):
    items=sorted(items,key=lambda i:i['x0'])
    return dict(y=items[0]['y'],text=' '.join(i['str'] for i in items).strip(),x0=min(i['x0'] for i in items),x1=max(i['x1'] for i in items))
def split_rows(rows,center,gapTol):
    lines=[]
    for r in rows:
        its=r['items']; bestGap=0; bestK=-1
        for k in range(len(its)-1):
            g=its[k+1]['x0']-its[k]['x1']
            if g>bestGap: bestGap=g; bestK=k
        if bestK>=0 and bestGap>gapTol and its[bestK]['x1']<=center<=its[bestK+1]['x0']:
            lines.append(row_to_line(its[:bestK+1])); lines.append(row_to_line(its[bestK+1:]))
        else: lines.append(row_to_line(its))
    return [l for l in lines if l['text']]

doc=fitz.open(sys.argv[1])
for p in [1,2]:
    items=extract(doc[p].get_text('words')); center=detect_split(items)
    gapTol=median([i['size'] for i in items])*1.2
    lines=split_rows(build_rows(items),center,gapTol)
    full=[l for l in lines if l['x0']<center<l['x1']]
    print(f"=== page {p+1} center={center:.0f} gapTol={gapTol:.1f} full={len(full)} ===")
    for l in sorted(full,key=lambda l:-l['y'])[:15]:
        spanw = l['x1']-l['x0']
        print(f"  y={l['y']:5.0f} x=[{l['x0']:5.1f},{l['x1']:5.1f}] w={spanw:5.1f}  {l['text'][:58]}")
