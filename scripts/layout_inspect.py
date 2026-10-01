import sys, fitz
doc = fitz.open(sys.argv[1])
for p in range(min(3, doc.page_count)):
    page = doc[p]
    words = page.get_text("words")  # x0,y0,x1,y1,word,block,line,word_no
    print(f"\n=== page {p+1} rect={page.rect} words={len(words)} ===")
    if not words:
        continue
    xs = [w[0] for w in words]
    x1s = [w[2] for w in words]
    minX, maxX = min(xs), max(x1s)
    B = 50
    hist = [0]*B
    for w in words:
        x0 = w[0]
        k = int((x0-minX)/(maxX-minX)*(B-1)); k = max(0,min(B-1,k))
        hist[k]+=1
    mx = max(hist)
    bar = ''.join('#' if v==mx else ('o' if v>0 else '.') for v in hist)
    print(f"x0 range {minX:.1f}..{maxX:.1f}")
    print("x0hist:", bar)
    # coverage histogram (interval overlap) like my planned detection
    C = 80
    cov = [0]*C
    for w in words:
        a = int((w[0]-minX)/(maxX-minX)*(C-1)); b = int((w[2]-minX)/(maxX-minX)*(C-1))
        for k in range(max(0,a), min(C-1,b)+1): cov[k]+=1
    cmx = max(cov)
    cbar = ''.join('#' if v>cmx*0.9 else ('o' if v>cmx*0.4 else ('+' if v>0 else '.')) for v in cov)
    print("cov  :", cbar, f"(max={cmx})")
    # 打印 y 最高的前 12 个词（自上而下第一行）
    ws = sorted(words, key=lambda w:-w[1])
    print("top line:", [(f"{w[4]}({w[0]:.0f},{w[1]:.0f})") for w in ws[:14]])
