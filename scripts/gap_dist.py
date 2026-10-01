import sys, fitz
from collections import Counter
doc=fitz.open(sys.argv[1])
word_gaps=[]  # gaps within same y (could be word gap or column gap)
sizes=[]
for p in range(doc.page_count):
    words=doc[p].get_text("words")
    words.sort(key=lambda w:(w[1], w[0]))  # y then x
    for i in range(1, len(words)):
        prev, cur = words[i-1], words[i]
        dy = abs(cur[1]-prev[1])
        if dy <= 1.5:  # same line
            gap = cur[0] - prev[2]  # x0(cur) - x1(prev)
            word_gaps.append(gap)
            sizes.append(cur[3]-cur[1])
# print distribution
import statistics
print("n same-line gaps:", len(word_gaps))
buckets = Counter()
for g in word_gaps:
    if g < 0: buckets['<0'] += 1
    elif g < 2: buckets['0-2'] += 1
    elif g < 4: buckets['2-4'] += 1
    elif g < 6: buckets['4-6'] += 1
    elif g < 8: buckets['6-8'] += 1
    elif g < 10: buckets['8-10'] += 1
    elif g < 14: buckets['10-14'] += 1
    elif g < 18: buckets['14-18'] += 1
    elif g < 24: buckets['18-24'] += 1
    elif g < 32: buckets['24-32'] += 1
    else: buckets['>32'] += 1
for k in ['<0','0-2','2-4','4-6','6-8','8-10','10-14','14-18','18-24','24-32','>32']:
    print(f"  gap {k:>6}: {buckets.get(k,0)}")
print("median font size:", statistics.median(sizes))
