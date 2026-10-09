# Which insider purchases still predict returns on liquid stocks? One factor at a time on 2018-22, then the test period.
import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, SizeBench, trades_vs
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load()); B = SizeBench(X)
ib = pd.read_parquet(f"{D}/insider_buys2.parquet").reset_index(drop=True)
ib = ib[(ib.usd >= 10_000) & (ib.lag_days.between(0, 10))]
dates = X.dates
ib["date"] = dates[np.minimum(dates.searchsorted(ib.filed), len(dates) - 1)]
# Prior 60-day return vs SPY at the signal date, and liquidity.
ret60 = (X.c / X.c.shift(60) - 1).sub(X.c["SPY"] / X.c["SPY"].shift(60) - 1, axis=0)
idx = pd.MultiIndex.from_arrays([ib.date, ib.sym])
ib["prior60"] = ret60.stack().reindex(idx).values
ib["dv20"] = X.dv20.stack().reindex(idx).values
ib = ib.dropna(subset=["dv20"])
ib = ib[ib.dv20 >= 10e6]
# Per stock-day aggregates: distinct insiders and dollars in the trailing 30 days (point in time).
ib = ib.sort_values("filed")
def trailing(g):
    f = g.filed.values; o = g.RPTOWNERCIK.values; u = g.usd.values
    n_ins, usd30 = [], []
    for k in range(len(g)):
        w = (f > f[k] - np.timedelta64(30, "D")) & (f <= f[k])
        n_ins.append(len(set(o[w]))); usd30.append(u[w].sum())
    return pd.DataFrame({"n_ins": n_ins, "usd30": usd30}, index=g.index)
ib = ib.join(pd.concat(trailing(g) for _, g in ib.groupby("sym")))
def stats(t):
    out = []
    for lo, hi in ((2018, 2022), (2023, 2026)):
        u = t[(t.date.dt.year >= lo) & (t.date.dt.year <= hi)]
        if len(u) < 40: out.append(f"{lo}-{hi%100}: n{len(u)}"); continue
        m = u.groupby(u.date.dt.to_period("M")).excess.mean()        # month clusters: holds overlap
        tt = m.mean() / (m.std() / np.sqrt(len(m)))
        out.append(f"{lo}-{hi%100}: {u.excess.mean()*100:+.2f}% t{tt:+.1f} n{len(u)}")
    return " | ".join(out)
def run(label, sub):
    ev = sub.assign(side=1)[["date", "sym", "side"]].drop_duplicates(["date", "sym"])
    print(f"{label:42s} " + "  ||  ".join(f"h{h}: {stats(trades_vs(X, B, ev, h))}" for h in (20, 60, 120)), flush=True)
run("ALL purchases", ib)
run("usd >= 100k", ib[ib.usd >= 1e5])
run("usd >= 500k", ib[ib.usd >= 5e5])
run("own_chg >= 10%", ib[ib.own_chg >= .10])
run("own_chg >= 50%", ib[ib.own_chg >= .50])
run("CEO/CFO", ib[ib.is_ceo_cfo])
run("officer", ib[ib.is_officer])
run("director only", ib[ib.is_director & ~ib.is_officer & ~ib.is_10pct])
run("10% owner", ib[ib.is_10pct])
run("prior60 <= -15% (bought the dip)", ib[ib.prior60 <= -.15])
run("prior60 <= -30%", ib[ib.prior60 <= -.30])
run("prior60 >= +15% (bought strength)", ib[ib.prior60 >= .15])
run("cluster 3+ insiders in 30d", ib[ib.n_ins >= 3])
run("dv20 >= 50M", ib[ib.dv20 >= 50e6])
run("dv20 10-50M", ib[ib.dv20 < 50e6])
run("direct holding", ib[ib.direct])
ib.to_parquet(f"{D}/insider_features.parquet")
