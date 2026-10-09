# Backtest the researched event signals on 2018-2026 liquid US stocks.
import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, summary, from_mask
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load())
HOLDS = [1, 3, 5, 10, 20]
exc = X.ret.sub(X.mret, axis=0)              # day-t return vs SPY
vmult = X.v / X.vavg20                        # day-t volume vs prior 20-day average
z = exc / X.vol20.shift(1)                    # shock in the stock's own volatility units
out = []
def run(label, ev):
    for h in HOLDS:
        out.append(summary(X.trades(ev, h), f"{label} h={h}")); print(out[-1], flush=True)

# 1. Volume-confirmed shocks (earnings/news days): follow the move.
for thr in (0.05, 0.08):
    up = (exc >= thr) & (vmult >= 3); dn = (exc <= -thr) & (vmult >= 3)
    run(f"shock+vol follow LONG  exc>={thr:.0%}", from_mask(up, 1))
    run(f"shock+vol follow SHORT exc<=-{thr:.0%}", from_mask(dn, -1))
# 2. Shocks on ordinary volume (no news proxy): fade the move.
up = (z >= 2.5) & (vmult < 1.5); dn = (z <= -2.5) & (vmult < 1.5)
run("quiet shock fade SHORT z>=2.5", from_mask(up, -1))
run("quiet shock fade LONG  z<=-2.5", from_mask(dn, 1))

# 3. Insider purchases.
ib = pd.read_parquet(f"{D}/insider_buys.parquet")
ib = ib[ib.usd >= 25_000]
dates = X.dates
ib["date"] = dates[np.minimum(dates.searchsorted(ib.filed), len(dates) - 1)]
ib["month"] = ib.traded.dt.month; ib["year"] = ib.traded.dt.year
# Cohen-Malloy-Pomorski routine: the same insider bought in the same calendar month in each of the 3 prior years.
seen = set(zip(ib.RPTOWNERCIK, ib.year, ib.month))
ib["routine"] = [all((o, y - k, m) in seen for k in (1, 2, 3)) for o, y, m in zip(ib.RPTOWNERCIK, ib.year, ib.month)]
officer = ib.RPTOWNER_TITLE.fillna("").str.contains("CEO|Chief Executive|CFO|Chief Financial|President", case=False)
def evs(df): return df.assign(side=1)[["date", "sym", "side"]].drop_duplicates(["date", "sym"])
run("insider any buy>=$25k", evs(ib))
run("insider opportunistic", evs(ib[~ib.routine]))
run("insider opportunistic CEO/CFO >=$100k", evs(ib[~ib.routine & officer & (ib.usd >= 100_000)]))
# Cluster: 2+ distinct insiders, $100k+ combined, within 10 days (signal on the filing that completes it).
ib = ib.sort_values("filed"); rows = []
for s, g in ib.groupby("sym"):
    f = g.filed.values; o = g.RPTOWNERCIK.values; u = g.usd.values; dt = g.date.values
    for k in range(len(g)):
        w = (f > f[k] - np.timedelta64(10, "D")) & (f <= f[k])
        if len(set(o[w])) >= 2 and u[w].sum() >= 100_000: rows.append((dt[k], s))
cl = pd.DataFrame(rows, columns=["date", "sym"]).drop_duplicates().assign(side=1)
run("insider cluster 2+ in 10d >=$100k", cl)
open(f"{D}/results.txt", "w").write("\n".join(out))
