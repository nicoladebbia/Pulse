# Variants for shortable (easy-to-borrow), liquid names; pick on 2018-2022, check on 2023-2026.
import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import robust as R   # reuses the panel, events and simulator (prints its own section first)
X, sim, etb = R.X, R.sim, R.etb
exc, vmult = R.exc, R.vmult
def stats(t):
    if t.empty: return "none"
    a = t[t.date.dt.year <= 2022]; b = t[t.date.dt.year >= 2023]
    def one(u):
        d = u.groupby("date").excess.mean(); ts = d.mean() / (d.std() / np.sqrt(len(d)))
        return f"{u.excess.mean()*100:+.2f}% (t {ts:+.1f}, n {len(u)})"
    return f"train18-22 {one(a)} | test23-26 {one(b)} | /day {len(t)/2200:.1f}"
for name, mask in (("UP-shock short", (exc >= 0.08) & (vmult >= 3)), ("DOWN-shock short", (exc <= -0.08) & (vmult >= 3)),
                   ("UP-shock short 5%", (exc >= 0.05) & (vmult >= 3)), ("DOWN-shock short 5%", (exc <= -0.05) & (vmult >= 3))):
    ev = R.from_mask(mask, -1); ev = ev[ev.sym.isin(etb)]
    dv = X.dv20.stack().reindex(pd.MultiIndex.from_arrays([ev.date, ev.sym])).values
    for floor in (10e6, 50e6):
        e = ev[dv >= floor]
        for mult, hard, mh in ((3, .15, 20), (np.inf, .15, 20), (np.inf, .25, 20), (np.inf, 9, 20), (np.inf, 9, 10), (5, .25, 20)):
            print(f"{name} dv>={floor/1e6:.0f}M trail={mult} hard={hard} hold={mh}: {stats(sim(e, mh, mult, hard))}", flush=True)
