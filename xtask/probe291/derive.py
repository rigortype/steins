import os
base = os.path.dirname(os.path.abspath(__file__))
rows = []
for mode in ("strict", "coercive"):
    with open(os.path.join(base, "witness-%s.tsv" % mode)) as fh:
        for line in fh:
            p = line.rstrip("\n").split("\t")
            if len(p) >= 5:
                rows.append(p)
cls = {
    "int": ["int"],
    "float": ["float(1.5)", "float(1.0)"],
    "string": ["string(numeric)", "string(non-numeric)"],
    "bool": ["bool(true)", "bool(false)"],
    "null": ["null"],
    "array": ["array"],
}
params = ["int", "float", "string", "bool", "?int", "int|string", "string|false", "DateTime"]
d = {(r[0], r[1], r[2]): r[4] for r in rows}
for mode in ("strict", "coercive"):
    print("\n== %s ==" % mode)
    print("base\\param\t" + "\t".join(params))
    for b, ws in cls.items():
        cells = []
        for p in params:
            vs = [d[(mode, p, w)] for w in ws]
            allerr = all(v == "TypeError" for v in vs)
            anyerr = any(v == "TypeError" for v in vs)
            cells.append("NO" if allerr else ("partial" if anyerr else "ok"))
        print(b + "\t" + "\t".join(cells))
