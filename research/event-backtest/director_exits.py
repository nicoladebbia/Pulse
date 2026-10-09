import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, SizeBench, sim_vs
from insider_combo import stats  # same month-clustered stats (re-runs its grid first; output ignored)
D = os.path.dirname(os.path.abspath(__file__))
import insider_combo as IC
X, B, ib = IC.X, IC.B, IC.ib
d = ib[ib.is_director & ~ib.is_officer & ~ib.is_10pct]
for min_usd in (10_000, 50_000):
    ev = d[d.usd >= min_usd].assign(side=1)[["date", "sym", "side"]].drop_duplicates(["date", "sym"])
    for mult, hard, mh in ((np.inf, .25, 60), (np.inf, .30, 60), (5, .25, 60), (6, .30, 60), (np.inf, .25, 40), (np.inf, .25, 80)):
        t = sim_vs(X, B, ev, mh, mult, hard)
        print(f"EXIT director usd>={min_usd/1e3:.0f}k trail={mult} hard={hard} hold={mh}: {stats(t)} | per day {len(ev)/2200:.2f}", flush=True)
