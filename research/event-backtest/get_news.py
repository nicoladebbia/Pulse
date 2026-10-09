# All Alpaca (Benzinga) news headlines with symbols, 2023-01-01 .. 2026-10-07, one file per month.
import os, time, requests, pandas as pd
H = {"APCA-API-KEY-ID": os.environ["ALPACA_API_KEY"], "APCA-API-SECRET-KEY": os.environ["ALPACA_SECRET_KEY"]}
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "news"); os.makedirs(OUT, exist_ok=True)
months = pd.date_range("2023-01-01", "2026-10-01", freq="MS")
for m in months:
    path = f"{OUT}/{m:%Y-%m}.parquet"
    if os.path.exists(path): continue
    end = min(m + pd.offsets.MonthBegin(1), pd.Timestamp("2026-10-07"))
    rows, tok = [], None
    while True:
        p = {"start": f"{m:%Y-%m-%d}T00:00:00Z", "end": f"{end:%Y-%m-%d}T00:00:00Z", "limit": 50, "sort": "asc", "include_content": "false"}
        if tok: p["page_token"] = tok
        for a in range(8):
            r = requests.get("https://data.alpaca.markets/v1beta1/news", params=p, headers=H, timeout=60)
            if r.status_code == 429: time.sleep(3 * (a + 1)); continue
            r.raise_for_status(); break
        d = r.json()
        rows += [(n["id"], n["created_at"], n["headline"], (n.get("summary") or "")[:300], ",".join(n.get("symbols") or []), n.get("source")) for n in d.get("news", [])]
        tok = d.get("next_page_token")
        if not tok: break
    pd.DataFrame(rows, columns=["id", "created_at", "headline", "summary", "symbols", "source"]).to_parquet(path)
    print(f"{m:%Y-%m}", len(rows), flush=True)
print("DONE")
