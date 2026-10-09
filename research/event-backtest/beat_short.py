import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import news_tests as N   # loads news + panel (and prints its grid; ignored)
import robust as R
X, n, etb = N.X, N.n, N.etb
m = n[n.headline.str.contains(N.RULES["beat_both"][1], regex=True)]
for timing in ("t_agg", "t_con"):
    ev = m.assign(date=m[timing], sym=m.symbols, side=-1)[["date", "sym", "side"]].drop_duplicates()
    ev = ev[ev.sym.isin(etb)]
    for hh in (5, 10, 20):
        print(f"BEAT-SHORT {timing} h={hh}: {N.stats(X.trades(ev, hh))}")
    for mult, hard in ((3, .15), (np.inf, .25)):
        t = R.sim(ev, 10, mult, hard)
        print(f"BEAT-SHORT {timing} SIM trail={mult} hard={hard} hold10: {N.stats(t)} worst1% {t.excess.quantile(.01)*100:.1f}%")
