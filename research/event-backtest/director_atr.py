import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import insider_combo as IC
from engine import sim_vs
X, B, ib, stats = IC.X, IC.B, IC.ib, IC.stats
tr = np.maximum(X.h - X.l, np.maximum((X.h - X.c.shift()).abs(), (X.l - X.c.shift()).abs()))
atrp = (tr.rolling(14, min_periods=10).mean() / X.c).stack()
d = ib[ib.is_director & ~ib.is_officer & ~ib.is_10pct].copy()
d["atrp"] = atrp.reindex(pd.MultiIndex.from_arrays([d.date, d.sym])).values
for name, sub in (("no ATR filter", d), ("ATR>=3%", d[d.atrp >= .03]), ("ATR<3%", d[d.atrp < .03])):
    ev = sub.assign(side=1)[["date", "sym", "side"]].drop_duplicates(["date", "sym"])
    t = sim_vs(X, B, ev, 60, 6, .30)
    print(f"ATRCHK {name:14s} {stats(t)} | per day {len(ev)/2200:.2f}", flush=True)
