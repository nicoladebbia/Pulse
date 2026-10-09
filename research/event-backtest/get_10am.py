# 10:00 ET price (open of the 10:00 minute bar) for every pre-market good/bad news event, 2023-2026.
import os, sys, time, requests, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from dip import tag, BAD, GOOD
D = os.path.dirname(os.path.abspath(__file__))
H = {"APCA-API-KEY-ID": os.environ["ALPACA_API_KEY"], "APCA-API-SECRET-KEY": os.environ["ALPACA_SECRET_KEY"]}
n = pd.read_parquet(f"{D}/news_events.parquet").reset_index(drop=True)
ts = pd.to_datetime(n.created_at, utc=True).dt.tz_convert("America/New_York")
mins = ts.dt.hour * 60 + ts.dt.minute
same_day = ts.dt.normalize().dt.tz_localize(None) == n.news_day
n = n[(same_day & (mins < 9 * 60 + 30)).values | (~same_day).values]         # known before that session's open
k = tag(n, {**GOOD, **BAD}); n = n[(k != "").values].assign(kind=k[k != ""])
n = n[~n.headline.str.contains("Top Ratings|Benzinga")]
n[["news_day", "symbols", "kind", "headline", "react"]].to_parquet(f"{D}/premarket_events.parquet")
out = f"{D}/px10.parquet"; rows = []
for day, g in n.groupby("news_day"):
    syms = sorted(set(g.symbols))
    t0 = pd.Timestamp(day.date()).tz_localize("America/New_York") + pd.Timedelta(hours=10)
    for i in range(0, len(syms), 100):
        p = {"symbols": ",".join(syms[i:i + 100]), "timeframe": "1Min", "start": t0.tz_convert("UTC").isoformat().replace("+00:00", "Z"),
             "end": (t0 + pd.Timedelta(minutes=5)).tz_convert("UTC").isoformat().replace("+00:00", "Z"), "feed": "sip", "adjustment": "all", "limit": 10000}
        for a in range(8):
            r = requests.get("https://data.alpaca.markets/v2/stocks/bars", params=p, headers=H, timeout=60)
            if r.status_code == 429: time.sleep(3 * (a + 1)); continue
            r.raise_for_status(); break
        for s, bars in (r.json().get("bars") or {}).items():
            if bars: rows.append((day, s, bars[0]["o"]))
pd.DataFrame(rows, columns=["date", "sym", "px10"]).to_parquet(out)
print("DONE", len(n), len(rows))
