# News events vs a size-matched benchmark, conditioned on how much the price already reacted on the news day.
import os, sys, glob, json, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, SizeBench, trades_vs
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load()); B = SizeBench(X)
etb = {a["symbol"] for a in json.load(open(f"{D}/assets.json")) if a.get("easy_to_borrow") and a.get("shortable") and a["status"] == "active"}
RULES = {
    "upgrade": (1, r"\bUpgrades\b.+\bto\b"), "downgrade": (-1, r"\bDowngrades\b.+\bto\b"),
    "beat_both": (1, r"EPS .*Beats.*Sales .*Beat"), "miss_both": (-1, r"EPS .*Misses.*Sales .*Miss"),
    "guide_up": (1, r"Raises (FY|Q\d|\d{4}|Full.Year|Annual|Its |Outlook|Guidance).{0,60}(Guidance|Outlook|Forecast|Sales|Revenue|EPS)|Sees .{0,60}Above .{0,20}Est"),
    "guide_down": (-1, r"(Lowers|Cuts|Reduces) (FY|Q\d|\d{4}|Full.Year|Annual|Its |Outlook|Guidance).{0,60}(Guidance|Outlook|Forecast|Sales|Revenue|EPS)|Sees .{0,60}Below .{0,20}Est"),
    "equity_offering": (-1, r"(Proposed|Pricing Of|Prices|Launches) .{0,40}(Public |Underwritten |Registered Direct )?Offering(?!.*Notes)"),
    "buyback": (1, r"(Announces|Authorizes|Approves|Board Approves).{0,40}(Buyback|Repurchase)"),
    "pt_raise": (1, r"Maintains .+ Raises Price Target"), "pt_lower": (-1, r"Maintains .+ Lowers Price Target"),
}
n = pd.concat(pd.read_parquet(p) for p in sorted(glob.glob(f"{D}/news/*.parquet"))).drop_duplicates("id")
n = n[(n.symbols.str.len() > 0) & ~n.symbols.str.contains(",")]
ts = pd.to_datetime(n.created_at, utc=True).dt.tz_convert("America/New_York")
mins = ts.dt.hour * 60 + ts.dt.minute
day = pd.to_datetime(ts.dt.date)
dates = X.dates
pos = dates.searchsorted(day)
is_td = (pos < len(dates)) & (dates[np.minimum(pos, len(dates) - 1)] == day)
# The "news day": the trading session the news first hits (after 16:00 or non-trading days -> next session).
news_i = np.where(is_td & (mins < 16 * 60), pos, np.where(is_td, pos + 1, pos))
ok = news_i < len(dates)
n = n[ok].assign(news_day=dates[news_i[ok]])
exc = X.ret.sub(X.mret, axis=0)
R = exc.stack()
n["react"] = R.reindex(pd.MultiIndex.from_arrays([n.news_day, n.symbols])).values
n = n.dropna(subset=["react"])
def stats(t):
    out = []
    for lo, hi in ((2023, 2024), (2025, 2026)):
        u = t[(t.date.dt.year >= lo) & (t.date.dt.year <= hi)]
        if len(u) < 30: out.append(f"{lo}-{hi%100}: n{len(u)}"); continue
        d = u.groupby("date").excess.mean(); tt = d.mean() / (d.std() / np.sqrt(len(d)))
        out.append(f"{lo}-{hi%100}: {u.excess.mean()*100:+.2f}% t{tt:+.1f} n{len(u)}")
    return " | ".join(out)
n.to_parquet(f"{D}/news_events.parquet")
for kind, (side, pat) in RULES.items():
    m = n[n.headline.str.contains(pat, regex=True) & ~n.headline.str.contains("Top Ratings|Benzinga")]
    for cond, sub in (("all", m), ("unreacted", m[side * m.react < 0.01]), ("reacted", m[side * m.react >= 0.03])):
        # Signal day = news day: enter at the next open.
        ev = sub.assign(date=sub.news_day, sym=sub.symbols, side=side)[["date", "sym", "side"]].drop_duplicates()
        if side < 0: ev = ev[ev.sym.isin(etb)]
        print(f"{kind:15s} {cond:9s} " + "  ||  ".join(f"h{h}: {stats(trades_vs(X, B, ev, h))}" for h in (3, 10, 20)), flush=True)
