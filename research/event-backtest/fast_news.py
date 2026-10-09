# Enter at 10:00 ET on the morning of pre-market news; exit at the close of day 0, 1, 5 or 10.
import os, sys, json, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, SizeBench, COST
from dip import GOOD, BAD
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load()); B = SizeBench(X)
etb = {a["symbol"] for a in json.load(open(f"{D}/assets.json")) if a.get("easy_to_borrow") and a.get("shortable") and a["status"] == "active"}
ev = pd.read_parquet(f"{D}/premarket_events.parquet").rename(columns={"news_day": "date", "symbols": "sym"})
px = pd.read_parquet(f"{D}/px10.parquet")
ev = ev.merge(px, on=["date", "sym"]).drop_duplicates(["date", "sym", "kind"])
ev["side"] = np.where(ev.kind.isin(list(GOOD)), 1, -1)
di = {d: i for i, d in enumerate(X.dates)}; cols = {s: j for j, s in enumerate(X.c.columns)}
O, C, Lq = X.o.values, X.c.values, X.liquid.values
rows = []
for r in ev.itertuples(index=False):
    i, j = di.get(r.date), cols.get(r.sym)
    if i is None or j is None or i < 1 or i + 10 >= len(X.dates) or not Lq[i - 1, j]: continue
    o, pc = O[i, j], C[i - 1, j]
    if not (o > 0 and pc > 0 and r.px10 > 0): continue
    gap, m30 = o / pc - 1, r.px10 / o - 1
    out = {"date": r.date, "sym": r.sym, "kind": r.kind, "side": r.side, "gap": gap, "m30": m30}
    for h in (0, 1, 5, 10):
        m = B.ret(i - 1, h + 1, j)
        out[f"x{h}"] = r.side * (C[i + h, j] / r.px10 - 1 - m) - COST if np.isfinite(m) else np.nan
    rows.append(out)
t = pd.DataFrame(rows)
t = t[(t.side > 0) | t.sym.isin(etb)]
def stats(u):
    res = []
    for lo, hi in ((2023, 2024), (2025, 2026)):
        v = u[(u.date.dt.year >= lo) & (u.date.dt.year <= hi)]
        cells = []
        for h in (0, 1, 5, 10):
            d = v.groupby("date")[f"x{h}"].mean().dropna()
            tt = d.mean() / (d.std() / np.sqrt(len(d))) if len(d) > 2 else np.nan
            cells.append(f"d{h} {v[f'x{h}'].mean()*100:+.2f}%(t{tt:+.1f})")
        res.append(f"{lo}-{hi%100} n{len(v)}: " + " ".join(cells))
    return " || ".join(res)
for kind in list(GOOD) + list(BAD):
    u = t[t.kind == kind]
    s = u.side.iloc[0] if len(u) else 1
    with_move = u[(s * u.gap > 0.01) & (s * u.m30 > 0)]        # opened in the news direction and kept going to 10:00
    against = u[(s * u.gap > 0.01) & (s * u.m30 < 0)]          # opened in the news direction, then faded by 10:00
    for name, v in (("all", u), ("gap+held", with_move), ("gap+faded", against)):
        print(f"{kind:10s} {name:9s} {stats(v)}", flush=True)
t.to_parquet(f"{D}/fast_news_trades.parquet")
