# Download daily SIP bars (split/dividend adjusted) for US-listed common-looking symbols, 2018 onward.
import os, re, time, json, sys, requests, pandas as pd
H = {"APCA-API-KEY-ID": os.environ["ALPACA_API_KEY"], "APCA-API-SECRET-KEY": os.environ["ALPACA_SECRET_KEY"]}
OUT = os.path.join(os.path.dirname(__file__), "bars"); os.makedirs(OUT, exist_ok=True)
assets = requests.get("https://paper-api.alpaca.markets/v2/assets", params={"asset_class": "us_equity"}, headers=H, timeout=60).json()
syms = sorted({a["symbol"] for a in assets if a["exchange"] in ("NYSE", "NASDAQ", "ARCA", "AMEX", "BATS") and re.fullmatch(r"[A-Z]{1,5}", a["symbol"])})
json.dump([{k: a.get(k) for k in ("symbol", "name", "exchange", "status", "shortable", "easy_to_borrow")} for a in assets if a["symbol"] in syms], open(os.path.join(OUT, "..", "assets.json"), "w"))
print(len(syms), "symbols", flush=True)
def get(params):
    for attempt in range(6):
        r = requests.get("https://data.alpaca.markets/v2/stocks/bars", params=params, headers=H, timeout=60)
        if r.status_code == 429: time.sleep(5 * (attempt + 1)); continue
        r.raise_for_status(); return r.json()
    raise RuntimeError("rate limited")
B = 100
for i in range(0, len(syms), B):
    path = os.path.join(OUT, f"b{i // B:04d}.parquet")
    if os.path.exists(path): continue
    batch = syms[i:i + B]; rows = []; tok = None
    while True:
        p = {"symbols": ",".join(batch), "timeframe": "1Day", "start": "2018-01-01", "end": "2026-10-07", "feed": "sip", "adjustment": "all", "limit": 10000}
        if tok: p["page_token"] = tok
        d = get(p)
        for s, bars in (d.get("bars") or {}).items():
            rows += [(s, b["t"][:10], b["o"], b["h"], b["l"], b["c"], b["v"]) for b in bars]
        tok = d.get("next_page_token")
        if not tok: break
    pd.DataFrame(rows, columns=["sym", "date", "o", "h", "l", "c", "v"]).to_parquet(path)
    print(i // B, len(rows), flush=True)
print("DONE")
