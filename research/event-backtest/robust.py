# Robustness of "short after a volume-confirmed up-shock".
import os, sys, json, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, summary, from_mask, COST
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load())
exc = X.ret.sub(X.mret, axis=0); vmult = X.v / X.vavg20
etb = {a["symbol"] for a in json.load(open(f"{D}/assets.json")) if a.get("easy_to_borrow") and a.get("shortable") and a["status"] == "active"}
up = (exc >= 0.08) & (vmult >= 3)
ev = from_mask(up, -1)
dv = X.dv20.stack(); px = X.c.stack()
ev["dv20"] = dv.reindex(pd.MultiIndex.from_arrays([ev.date, ev.sym])).values
ev["px"] = px.reindex(pd.MultiIndex.from_arrays([ev.date, ev.sym])).values
for h in (5, 10, 20):
    print(summary(X.trades(ev, h), f"base h={h}").split(" | ")[0])
h = 20
for lo, hi in ((10e6, 50e6), (50e6, 200e6), (200e6, 1e13)):
    print(summary(X.trades(ev[(ev.dv20 >= lo) & (ev.dv20 < hi)], h), f"dv20 ${lo/1e6:.0f}M-{hi/1e6:.0f}M h=20"))
for lo, hi in ((5, 20), (20, 1e9)):
    print(summary(X.trades(ev[(ev.px >= lo) & (ev.px < hi)], h), f"price ${lo}-{hi} h=20").split(" | ")[0])
e2 = ev[ev.sym.isin(etb)]
print(summary(X.trades(e2, h), "easy-to-borrow today h=20"))
print(summary(X.trades(e2[e2.dv20 >= 50e6], h), "ETB & dv20>=$50M h=20"))

# Path simulation with the bot's short exits: +15% hard stop, 3x ATR(14) trailing stop above the low, max hold.
def sim(events, maxh, atr_mult=3.0, hard=0.15):
    di = {d: i for i, d in enumerate(X.dates)}; cols = {s: j for j, s in enumerate(X.c.columns)}
    O, H, L, C = X.o.values, X.h.values, X.l.values, X.c.values
    tr = np.maximum(X.h - X.l, np.maximum((X.h - X.c.shift()).abs(), (X.l - X.c.shift()).abs()))
    ATR = tr.rolling(14, min_periods=10).mean().values
    so, sc = X.spy_o.values, X.spy_c.values; Lq = X.liquid.values; n = len(X.dates); rows = []
    for d, s in zip(events.date, events.sym):
        i, j = di.get(d), cols.get(s)
        if i is None or j is None or i + maxh >= n or not Lq[i, j]: continue
        e = O[i + 1, j]; a = ATR[i, j]
        if not (e > 0 and a > 0): continue
        low = e; exit_px = None; k_exit = i + maxh
        for k in range(i + 1, i + maxh + 1):
            stop = min(low + atr_mult * a, e * (1 + hard))
            if k > i + 1 and O[k, j] >= stop: exit_px, k_exit = O[k, j], k; break      # gapped through
            if H[k, j] >= stop: exit_px, k_exit = stop, k; break
            low = min(low, L[k, j])
        if exit_px is None: exit_px = C[i + maxh, j]
        if not exit_px > 0: continue
        r = -(exit_px / e - 1); m = -(sc[k_exit] / so[i + 1] - 1)
        rows.append((d, s, r - m - COST, r - COST, k_exit - i))
    return pd.DataFrame(rows, columns=["date", "sym", "excess", "raw", "days"])
for name, sub in (("all", ev), ("ETB & dv20>=$50M", e2[e2.dv20 >= 50e6])):
    for mh in (10, 20):
        t = sim(sub, mh)
        print(summary(t, f"SIM bot exits {name} maxhold={mh}").split(" | ")[0], f"avg days={t.days.mean():.1f} worst1%={t.excess.quantile(.01)*100:.1f}% total raw/yr~{t.raw.mean()*len(t)/8.75:.1f}x-notional")
    print(" by year", t.groupby(t.date.dt.year).excess.mean().mul(100).round(2).to_dict())
