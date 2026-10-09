import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, SizeBench, trades_vs, sim_vs
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load()); B = SizeBench(X)
ib = pd.read_parquet(f"{D}/insider_features.parquet")
def stats(t):
    out = []
    for lo, hi in ((2018, 2022), (2023, 2026), (2018, 2026)):
        u = t[(t.date.dt.year >= lo) & (t.date.dt.year <= hi)]
        m = u.groupby(u.date.dt.to_period("M")).excess.mean(); tt = m.mean() / (m.std() / np.sqrt(len(m)))
        out.append(f"{lo}-{hi%100}: {u.excess.mean()*100:+.2f}% med {u.excess.median()*100:+.2f}% t{tt:+.1f} n{len(u)}")
    return " | ".join(out)
combos = {
    "director-only": ib[ib.is_director & ~ib.is_officer & ~ib.is_10pct],
    "dv 10-50M": ib[ib.dv20 < 50e6],
    "director-only & dv 10-50M": ib[ib.is_director & ~ib.is_officer & ~ib.is_10pct & (ib.dv20 < 50e6)],
    "not officer & not 10%": ib[~ib.is_officer & ~ib.is_10pct],
}
for name, sub in combos.items():
    ev = sub.assign(side=1)[["date", "sym", "side"]].drop_duplicates(["date", "sym"])
    for h in (40, 60, 90):
        print(f"{name:28s} hold {h}: {stats(trades_vs(X, B, ev, h))}", flush=True)
    for mult, hard in ((3, .15), (5, .25), (np.inf, 1.0)):
        t = sim_vs(X, B, ev, 60, mult, hard)
        print(f"{name:28s} SIM 60d trail={mult} hard={hard}: {stats(t)} avg days {t.days.mean():.0f}", flush=True)
