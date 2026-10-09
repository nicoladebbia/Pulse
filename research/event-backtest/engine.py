# Event backtest: signal known after the close of day t, enter at the open of t+1,
# exit at the close of t+h. Excess vs SPY over the same window, minus round-trip cost.
import numpy as np, pandas as pd
COST = 0.002  # 10 bp per side

class Panel:
    def __init__(self, P):
        self.o, self.h, self.l, self.c, self.v = (P[k] for k in "ohlcv")
        self.dates = self.c.index
        self.dv20 = (self.c * self.v).rolling(20, min_periods=15).mean()
        self.liquid = (self.c >= 5) & (self.dv20 >= 10e6)
        self.ret = self.c.pct_change(fill_method=None)
        self.vol20 = self.ret.rolling(20, min_periods=15).std()
        self.vavg20 = self.v.rolling(20, min_periods=15).mean().shift(1)
        self.spy_o, self.spy_c = self.o["SPY"], self.c["SPY"]
        self.mret = self.ret["SPY"]

    def trades(self, events, h):
        """events: DataFrame(date, sym, side) with side +1 long / -1 short."""
        di = {d: i for i, d in enumerate(self.dates)}
        cols = {s: j for j, s in enumerate(self.c.columns)}
        O, C = self.o.values, self.c.values
        so, sc = self.spy_o.values, self.spy_c.values
        L = self.liquid.values
        n = len(self.dates); rows = []
        for d, s, side in events[["date", "sym", "side"]].itertuples(index=False):
            i, j = di.get(d), cols.get(s)
            if i is None or j is None or i + h >= n or not L[i, j]: continue
            eo, xc = O[i + 1, j], C[i + h, j]
            if not (eo > 0 and xc > 0): continue
            r = xc / eo - 1; m = sc[i + h] / so[i + 1] - 1
            rows.append((d, s, side, side * (r - m) - COST, side * r - COST))
        return pd.DataFrame(rows, columns=["date", "sym", "side", "excess", "raw"])

def summary(t, label):
    if t.empty: return f"{label}: no trades"
    daily = t.groupby("date").excess.mean()
    tstat = daily.mean() / (daily.std(ddof=1) / np.sqrt(len(daily))) if len(daily) > 2 else float("nan")
    yrs = t.groupby(t.date.dt.year).excess.mean().mul(100).round(2).to_dict()
    span = (t.date.max() - t.date.min()).days / 365.25 * 252 or 1
    return (f"{label}: n={len(t)} ({len(t)/span:.1f}/day) mean={t.excess.mean()*100:+.2f}% med={t.excess.median()*100:+.2f}% "
            f"hit={(t.excess>0).mean()*100:.0f}% t(day-clustered)={tstat:+.2f} | by year {yrs}")

def from_mask(mask, side):
    st = mask.stack(); st = st[st]
    return pd.DataFrame({"date": st.index.get_level_values(0), "sym": st.index.get_level_values(1), "side": side})
