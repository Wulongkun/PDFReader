import sys, fitz, io, statistics
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')

def median(a):
    if not a: return 0
    s=sorted(a); m=len(s)//2
    return s[m] if len(s)%2 else (s[m-1]+s[m])/2

def extract(items_raw):
    out=[]
    for w in items_raw:
        x0,y0,x1,y1,txt=w[0],w[1],w[2],w[3],w[4]
        if not txt.strip(): continue
        out.append(dict(str=txt, x0=x0, x1=x1, y=y0, size=y1-y0))
    return out

def detect_split(items):
    n=len(items)
    if n<12: return None
    minX=min(i['x0'] for i in items); maxX=max(i['x1'] for i in items)
    width=maxX-minX
    if width<=0: return None
    B=120
    edge=[0]*B
    for i in items:
        a=max(0,min(B-1,int((i['x0']-minX)/width*(B-1)))); edge[a]+=1
        b=max(0,min(B-1,int((i['x1']-minX)/width*(B-1)))); edge[b]+=1
    lo,hi=int(B*0.15),int(B*0.85)
    thresh=max(2,n*0.01)
    gaps=[]; run=-1
    for k in range(lo,hi+1):
        if edge[k]<=thresh:
            if run<0: run=k
        else:
            if run>=0:
                if k-run>=2: gaps.append([run,k-run])
                run=-1
    if run>=0 and hi+1-run>=2: gaps.append([run,hi+1-run])
    if not gaps: return None
    centerPx=minX+width/2
    best=None
    for gs,L in gaps:
        c=minX+(gs+L/2)/B*width
        left=sum(edge[:gs]); right=sum(edge[gs+L:])
        if left<n*0.3 or right<n*0.3: continue
        d=abs(c-centerPx)
        if best is None or d<best[0]: best=(d,c)
    return best[1] if best else None

def build_rows(items):
    items=sorted(items, key=lambda i:(i['y'], i['x0']))
    if not items: return []
    ytol=median([i['size'] for i in items])*0.5
    rows=[]
    for it in items:
        if rows and abs(rows[-1]['y']-it['y'])<=ytol:
            rows[-1]['items'].append(it)
            rows[-1]['y']=(rows[-1]['y']*(len(rows[-1]['items'])-1)+it['y'])/len(rows[-1]['items'])
        else:
            rows.append(dict(y=it['y'], items=[it]))
    for r in rows: r['items'].sort(key=lambda i:i['x0'])
    return rows

def row_to_line(items):
    items=sorted(items,key=lambda i:i['x0'])
    return dict(y=items[0]['y'], text=' '.join(i['str'] for i in items).strip(),
                x0=min(i['x0'] for i in items), x1=max(i['x1'] for i in items),
                size=max(i['size'] for i in items))

def split_rows(rows, center, gapTol):
    lines=[]
    for r in rows:
        its=r['items']
        # 找该行最大空隙
        bestGap=0; bestK=-1
        for k in range(len(its)-1):
            g=its[k+1]['x0']-its[k]['x1']
            if g>bestGap: bestGap=g; bestK=k
        # 若最大空隙跨过栏缝且足够宽 → 拆成左右两行
        if bestK>=0 and bestGap>gapTol and its[bestK]['x1']<=center<=its[bestK+1]['x0']:
            lines.append(row_to_line(its[:bestK+1]))
            lines.append(row_to_line(its[bestK+1:]))
        else:
            lines.append(row_to_line(its))
    return [l for l in lines if l['text']]

doc=fitz.open(sys.argv[1])
for p in range(min(6, doc.page_count)):
    items=extract(doc[p].get_text('words'))
    center=detect_split(items)
    if not center:
        print(f"p{p+1}: single"); continue
    rows=build_rows(items)
    gapTol=median([i['size'] for i in items])*1.2
    lines=split_rows(rows, center, gapTol)
    full=[l for l in lines if l['x0']<center<l['x1']]
    left=[l for l in lines if not(l['x0']<center<l['x1']) and (l['x0']+l['x1'])/2<center]
    right=[l for l in lines if not(l['x0']<center<l['x1']) and (l['x0']+l['x1'])/2>=center]
    print(f"p{p+1}: center={center:.0f} lines={len(lines)} full={len(full)} left={len(left)} right={len(right)}")
    print(f"  full[0..3]:", [l['text'][:28] for l in full[:4]])
    print(f"  left[0..2]:", [l['text'][:28] for l in left[:3]])
    print(f"  right[0..2]:", [l['text'][:28] for l in right[:3]])
    # 检查 left/right 是否仍有"跨栏"行(即整行横跨两栏) —— 应极少
    mixed_left=[l for l in left if l['x0']<center-5 and l['x1']>center+5]
    mixed_right=[l for l in right if l['x0']<center-5 and l['x1']>center+5]
    print(f"  mixed_left={len(mixed_left)} mixed_right={len(mixed_right)}")
