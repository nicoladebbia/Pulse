# Headline-classified news events (Benzinga via Alpaca), 2023-2026, liquid US stocks.
import os, sys, glob, json, re, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load())
etb = {a["symbol"] for a in json.load(open(f"{D}/assets.json")) if a.get("easy_to_borrow") and a.get("shortable") and a["status"] == "active"}
n = pd.concat(pd.read_parquet(p) for p in sorted(glob.glob(f"{D}/news/*.parquet")))
n = n[n.symbols.str.len() > 0]
n = n[~n.symbols.str.contains(",")]                      # one stock per headline
h = n.headline
RULES = {
    "upgrade": (1, r"\bUpgrades\b.+\bto\b"),
    "downgrade": (-1, r"\bDowngrades\b.+\bto\b"),
    "beat_both": (1, r"EPS .*Beats.*Sales .*Beat"),
    "miss_both": (-1, r"EPS .*Misses.*Sales .*Miss"),
    "guide_up": (1, r"Raises (FY|Q\d|\d{4}|Full.Year|Annual|Its |Outlook|Guidance).{0,60}(Guidance|Outlook|Forecast|Sales|Revenue|EPS)|Sees .{0,60}Above .{0,20}Est"),
    "guide_down": (-1, r"(Lowers|Cuts|Reduces) (FY|Q\d|\d{4}|Full.Year|Annual|Its |Outlook|Guidance).{0,60}(Guidance|Outlook|Forecast|Sales|Revenue|EPS)|Sees .{0,60}Below .{0,20}Est"),
    "equity_offering": (-1, r"(Proposed|Pricing Of|Prices|Launches) .{0,40}(Public |Underwritten |Registered Direct )?Offering(?!.*Notes)"),
    "initiate_buy": (1, r"Initiates Coverage On .+ With (Buy|Overweight|Outperform|Strong Buy)"),
    "initiate_sell": (-1, r"Initiates Coverage On .+ With (Sell|Underweight|Underperform)"),
    "buyback": (1, r"(Announces|Authorizes|Approves|Board Approves).{0,40}(Buyback|Repurchase)"),
    "pt_raise": (1, r"Maintains .+ Raises Price Target"),
    "pt_lower": (-1, r"Maintains .+ Lowers Price Target"),
}
ts = pd.to_datetime(n.created_at, utc=True).dt.tz_convert("America/New_York")
dates = X.dates
day = pd.to_datetime(ts.dt.date)
pre_open = (ts.dt.hour * 60 + ts.dt.minute) < 9 * 60 + 30
pos = dates.searchsorted(day)                            # first trading day >= news day
is_td = (pos < len(dates)) & (dates[np.minimum(pos, len(dates) - 1)] == day)
# Aggressive: known before the open -> enter at that open (signal day = previous trading day).
agg = np.where(pre_open & is_td, pos - 1, np.where(is_td, pos, pos - 1))
# Conservative: always enter the trading day after the news day.
con = np.where(is_td, pos, pos - 1)
ok = (agg >= 0) & (pos < len(dates))
n = n[ok].assign(t_agg=dates[agg[ok]], t_con=dates[np.minimum(con[ok], len(dates) - 1)])
def stats(t):
    out = []
    for lo, hi in ((2023, 2024), (2025, 2026)):
        u = t[(t.date.dt.year >= lo) & (t.date.dt.year <= hi)]
        if len(u) < 30: out.append(f"{lo}-{hi % 100}: n {len(u)}"); continue
        d = u.groupby("date").excess.mean(); tt = d.mean() / (d.std() / np.sqrt(len(d)))
        out.append(f"{lo}-{hi % 100}: {u.excess.mean()*100:+.2f}% t{tt:+.1f} n{len(u)}")
    return " | ".join(out) + f" | /day {len(t)/940:.1f}"
for kind, (side, pat) in RULES.items():
    m = n[n.headline.str.contains(pat, regex=True)]
    if kind in ("upgrade", "downgrade"): m = m[~m.headline.str.contains("Top Ratings|Benzinga")]
    for timing in ("t_agg", "t_con"):
        ev = m.assign(date=m[timing], sym=m.symbols, side=side)[["date", "sym", "side"]].drop_duplicates()
        if side < 0: ev = ev[ev.sym.isin(etb)]
        for hh in (1, 3, 5, 10, 20):
            print(f"{kind:16s} {timing} h={hh:2d}: {stats(X.trades(ev, hh))}", flush=True)
