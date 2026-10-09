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

class SizeBench:
    """Benchmark = equal-weight return of liquid stocks in the same dollar-volume quintile over the same window.
    SPY is a mega-cap index; small and mid caps lagged it by a lot in 2023-25, which makes any
    small/mid-cap signal look bad against SPY."""
    def __init__(self, X):
        self.X = X; self.cache = {}
        dv = X.dv20.where(X.liquid)
        self.q = dv.rank(axis=1, pct=True).mul(5).clip(upper=4.999).apply(np.floor)   # 0..4 per date
        self.O, self.C, self.Q = X.o.values, X.c.values, self.q.values
    def ret(self, i, h, j):
        b = self.Q[i, j]
        if not np.isfinite(b): return np.nan
        key = (i, h, int(b))
        if key not in self.cache:
            cols = self.Q[i] == b
            r = self.C[i + h, cols] / self.O[i + 1, cols] - 1
            r = r[np.isfinite(r)]
            self.cache[key] = np.clip(r, -0.9, 3).mean() if len(r) else np.nan
        return self.cache[key]

def trades_vs(X, B, events, h):
    di = {d: i for i, d in enumerate(X.dates)}; cols = {s: j for j, s in enumerate(X.c.columns)}
    O, C, L = X.o.values, X.c.values, X.liquid.values; n = len(X.dates); rows = []
    for d, s, side in events[["date", "sym", "side"]].itertuples(index=False):
        i, j = di.get(d), cols.get(s)
        if i is None or j is None or i + h >= n or not L[i, j]: continue
        eo, xc = O[i + 1, j], C[i + h, j]
        if not (eo > 0 and xc > 0): continue
        m = B.ret(i, h, j)
        if not np.isfinite(m): continue
        r = xc / eo - 1
        rows.append((d, s, side, side * (r - m) - COST, side * r - COST))
    return pd.DataFrame(rows, columns=["date", "sym", "side", "excess", "raw"])

def sim_vs(X, B, events, maxh, atr_mult=3.0, hard=0.15):
    """Path simulation with the bot's exits, either side: trailing stop atr_mult x ATR(14) from the best price,
    hard stop at `hard`, else close at maxh. Benchmark: size-matched over the same window."""
    di = {d: i for i, d in enumerate(X.dates)}; cols = {s: j for j, s in enumerate(X.c.columns)}
    O, H, L_, C = X.o.values, X.h.values, X.l.values, X.c.values
    if not hasattr(X, "_atr"):
        tr = np.maximum(X.h - X.l, np.maximum((X.h - X.c.shift()).abs(), (X.l - X.c.shift()).abs()))
        X._atr = tr.rolling(14, min_periods=10).mean().values
    A, Lq, n, rows = X._atr, X.liquid.values, len(X.dates), []
    for d, s, side in events[["date", "sym", "side"]].itertuples(index=False):
        i, j = di.get(d), cols.get(s)
        if i is None or j is None or i + maxh >= n or not Lq[i, j]: continue
        e, a = O[i + 1, j], A[i, j]
        if not (e > 0 and a > 0): continue
        best, px, k_exit = e, None, i + maxh
        for k in range(i + 1, i + maxh + 1):
            if side > 0:
                stop = max(best - atr_mult * a, e * (1 - hard))
                if k > i + 1 and O[k, j] <= stop: px, k_exit = O[k, j], k; break
                if L_[k, j] <= stop: px, k_exit = stop, k; break
                best = max(best, H[k, j])
            else:
                stop = min(best + atr_mult * a, e * (1 + hard))
                if k > i + 1 and O[k, j] >= stop: px, k_exit = O[k, j], k; break
                if H[k, j] >= stop: px, k_exit = stop, k; break
                best = min(best, L_[k, j])
        if px is None: px = C[i + maxh, j]
        m = B.ret(i, k_exit - i, j)
        if not (px > 0 and np.isfinite(m)): continue
        r = px / e - 1
        rows.append((d, s, side, side * (r - m) - COST, side * r - COST, k_exit - i))
    return pd.DataFrame(rows, columns=["date", "sym", "side", "excess", "raw", "days"])
