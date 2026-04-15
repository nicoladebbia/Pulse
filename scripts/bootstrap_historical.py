#!/usr/bin/env python3
"""
Pulse Historical Data Bootstrap — Download 2 years of SEC filings + prices.

Downloads Form 4 and 8-K filings for S&P 500 companies from SEC EDGAR EFTS,
plus daily price candles from Finnhub. Stores in pulse_historical.db.

Usage:
    python3 scripts/bootstrap_historical.py [--tickers AAPL,MSFT,NVDA] [--days 730]
    python3 scripts/bootstrap_historical.py --top 100  # Top 100 by market cap
    python3 scripts/bootstrap_historical.py --prices-only  # Just fetch prices

Requires: FINNHUB_API_KEY env var for prices.
SEC EDGAR requires no key (just User-Agent).
"""

import argparse
import json
import os
import sqlite3
import sys
import time
import urllib.request
import urllib.error
from datetime import datetime, timedelta
from typing import Optional

SEC_USER_AGENT = "Pulse/1.0 (pulse-app@example.com)"
DB_PATH = os.path.join(os.path.dirname(__file__), "..", "pulse_historical.db")

# --- Database Setup ---

def init_db(db_path: str) -> sqlite3.Connection:
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")

    conn.executescript("""
    CREATE TABLE IF NOT EXISTS companies (
        cik TEXT PRIMARY KEY,
        ticker TEXT NOT NULL,
        name TEXT NOT NULL,
        UNIQUE(ticker)
    );

    CREATE TABLE IF NOT EXISTS filings (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        cik TEXT NOT NULL,
        ticker TEXT,
        form_type TEXT NOT NULL,
        accession TEXT NOT NULL UNIQUE,
        file_date TEXT NOT NULL,
        entity_name TEXT,
        -- Form 4 fields
        transaction_code TEXT,
        shares REAL DEFAULT 0,
        price_per_share REAL DEFAULT 0,
        total_value REAL DEFAULT 0,
        is_officer INTEGER DEFAULT 0,
        is_director INTEGER DEFAULT 0,
        officer_title TEXT,
        owner_name TEXT,
        trade_classification TEXT,
        signal_weight REAL DEFAULT 0,
        -- 8-K fields
        item_numbers TEXT,
        event_classification TEXT,
        event_severity REAL DEFAULT 0,
        raw_metadata TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_filings_cik_date ON filings(cik, file_date);
    CREATE INDEX IF NOT EXISTS idx_filings_ticker_date ON filings(ticker, file_date);
    CREATE INDEX IF NOT EXISTS idx_filings_form ON filings(form_type, file_date);

    CREATE TABLE IF NOT EXISTS daily_prices (
        ticker TEXT NOT NULL,
        date TEXT NOT NULL,
        open REAL,
        high REAL,
        low REAL,
        close REAL NOT NULL,
        volume INTEGER,
        change_1d REAL,
        UNIQUE(ticker, date)
    );
    CREATE INDEX IF NOT EXISTS idx_prices_ticker ON daily_prices(ticker, date);

    CREATE TABLE IF NOT EXISTS bootstrap_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        action TEXT NOT NULL,
        detail TEXT,
        count INTEGER DEFAULT 0,
        created_at TEXT DEFAULT (datetime('now'))
    );
    """)
    conn.commit()
    return conn


# --- SEC EDGAR Downloads ---

def fetch_sec_json(url: str) -> Optional[dict]:
    """Fetch JSON from SEC with proper User-Agent."""
    req = urllib.request.Request(url)
    req.add_header("User-Agent", SEC_USER_AGENT)
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode())
    except (urllib.error.HTTPError, urllib.error.URLError, json.JSONDecodeError) as e:
        print(f"  SEC fetch error: {e}")
        return None


def fetch_sec_text(url: str) -> Optional[str]:
    """Fetch text/HTML from SEC."""
    req = urllib.request.Request(url)
    req.add_header("User-Agent", SEC_USER_AGENT)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.read().decode("utf-8", errors="replace")
    except Exception:
        return None


def load_sp500_tickers(top_n: int = 500) -> list[tuple[str, str, str]]:
    """Load company tickers from SEC's company_tickers.json. Returns [(cik, ticker, name)]."""
    print("Downloading SEC company_tickers.json...")
    data = fetch_sec_json("https://www.sec.gov/files/company_tickers.json")
    if not data:
        print("ERROR: Could not download company tickers")
        sys.exit(1)

    companies = []
    for entry in data.values():
        companies.append((
            str(entry["cik_str"]).zfill(10),
            entry["ticker"],
            entry["title"],
        ))

    # Sort by CIK (roughly by size/age) and take top N
    companies.sort(key=lambda x: int(x[0]))
    return companies[:top_n]


def download_filings(conn: sqlite3.Connection, form_type: str, start_date: str, end_date: str,
                     tickers: Optional[list[str]] = None, max_total: int = 5000):
    """Download filings per company from SEC data.sec.gov submissions API."""
    companies = conn.execute("SELECT cik, ticker, name FROM companies").fetchall()
    print(f"\nDownloading {form_type} filings for {len(companies)} companies ({start_date} to {end_date})...")

    total_stored = 0
    errors = 0

    for i, (cik, ticker, name) in enumerate(companies):
        if total_stored >= max_total:
            break

        # SEC submissions API: returns all recent filings for a company
        padded_cik = cik.zfill(10)
        url = f"https://data.sec.gov/submissions/CIK{padded_cik}.json"

        data = fetch_sec_json(url)
        if not data:
            errors += 1
            time.sleep(0.12)
            continue

        recent = data.get("filings", {}).get("recent", {})
        forms = recent.get("form", [])
        dates = recent.get("filingDate", [])
        accessions = recent.get("accessionNumber", [])
        names = recent.get("primaryDocDescription", [""] * len(forms))

        company_count = 0
        for j in range(len(forms)):
            if forms[j] != form_type:
                continue
            if j >= len(dates) or j >= len(accessions):
                continue
            file_date = dates[j]
            if file_date < start_date or file_date > end_date:
                continue

            accession = accessions[j]
            exists = conn.execute("SELECT 1 FROM filings WHERE accession = ?", (accession,)).fetchone()
            if exists:
                continue

            conn.execute(
                """INSERT OR IGNORE INTO filings (cik, ticker, form_type, accession, file_date, entity_name)
                   VALUES (?, ?, ?, ?, ?, ?)""",
                (cik, ticker, form_type, accession, file_date, name)
            )
            company_count += 1
            total_stored += 1

        if (i + 1) % 10 == 0:
            conn.commit()
            print(f"  {form_type}: {i+1}/{len(companies)} companies, {total_stored} filings")

        time.sleep(0.12)  # SEC rate limit: 10 req/sec

    conn.commit()
    print(f"  {form_type}: {total_stored} total filings for {len(companies)} companies ({errors} errors)")
    conn.execute(
        "INSERT INTO bootstrap_log (action, detail, count) VALUES (?, ?, ?)",
        (f"download_{form_type}", f"{start_date} to {end_date}", total_stored)
    )
    conn.commit()
    return total_stored


def enrich_form4_filings(conn: sqlite3.Connection, limit: int = 2000):
    """Download Form 4 XMLs and parse transaction details."""
    print("\nEnriching Form 4 filings with transaction data...")

    rows = conn.execute(
        """SELECT id, cik, accession FROM filings
           WHERE form_type = '4' AND transaction_code IS NULL
           ORDER BY file_date DESC LIMIT ?""",
        (limit,)
    ).fetchall()

    enriched = 0
    errors = 0

    for filing_id, cik, accession in rows:
        accession_nd = accession.replace("-", "")
        cik_clean = cik.lstrip("0") or "0"
        xml_url = f"https://www.sec.gov/Archives/edgar/data/{cik_clean}/{accession_nd}/{accession_nd}.xml"

        xml = fetch_sec_text(xml_url)
        if not xml:
            # Try finding XML via index page (XML filename varies per filing)
            index_url = f"https://www.sec.gov/Archives/edgar/data/{cik_clean}/{accession_nd}/"
            index_html = fetch_sec_text(index_url)
            if index_html:
                for part in index_html.split('href="')[1:]:
                    href = part.split('"')[0]
                    if href.endswith(".xml") and "R1" not in href and "R2" not in href and "index" not in href.lower():
                        # href may be absolute (/Archives/...) or relative
                        if href.startswith("/"):
                            full_url = f"https://www.sec.gov{href}"
                        elif href.startswith("http"):
                            full_url = href
                        else:
                            full_url = f"{index_url}{href}"
                        xml = fetch_sec_text(full_url)
                        if xml and "<ownershipDocument" in xml:
                            break
                        xml = None  # Not a Form 4 XML, keep looking
            time.sleep(0.12)

        if not xml:
            errors += 1
            if errors > 50:
                print(f"  Too many errors ({errors}), stopping enrichment")
                break
            continue

        # Parse Form 4 XML
        def safe_float(val: Optional[str]) -> float:
            if not val:
                return 0.0
            try:
                return float(val)
            except (ValueError, TypeError):
                return 0.0

        txn_code = extract_xml_value(xml, "transactionCode") or "?"
        shares = safe_float(extract_nested_value(xml, "transactionShares"))
        price = safe_float(extract_nested_value(xml, "transactionPricePerShare"))
        total_value = shares * price
        owner_name = extract_xml_value(xml, "rptOwnerName") or ""
        is_officer = 1 if "<isOfficer>1</isOfficer>" in xml or "<isOfficer>true</isOfficer>" in xml else 0
        is_director = 1 if "<isDirector>1</isDirector>" in xml or "<isDirector>true</isDirector>" in xml else 0
        officer_title = extract_xml_value(xml, "officerTitle") or ""
        post_shares = safe_float(extract_nested_value(xml, "sharesOwnedFollowingTransaction"))

        # Classify
        classification, signal_weight = classify_form4(txn_code, is_officer, is_director, total_value, shares, post_shares)

        conn.execute(
            """UPDATE filings SET
               transaction_code=?, shares=?, price_per_share=?, total_value=?,
               is_officer=?, is_director=?, officer_title=?, owner_name=?,
               trade_classification=?, signal_weight=?
               WHERE id=?""",
            (txn_code, shares, price, total_value,
             is_officer, is_director, officer_title, owner_name,
             classification, signal_weight, filing_id)
        )
        enriched += 1

        if enriched % 50 == 0:
            conn.commit()
            print(f"  Enriched {enriched}/{len(rows)} Form 4s")

        time.sleep(0.12)  # SEC rate limit

    conn.commit()
    print(f"  Form 4 enrichment: {enriched} enriched, {errors} errors")
    return enriched


def enrich_8k_filings(conn: sqlite3.Connection, limit: int = 1000):
    """Download 8-K HTMLs and extract Item numbers."""
    print("\nEnriching 8-K filings with event classification...")

    rows = conn.execute(
        """SELECT id, cik, accession FROM filings
           WHERE form_type = '8-K' AND event_classification IS NULL
           ORDER BY file_date DESC LIMIT ?""",
        (limit,)
    ).fetchall()

    enriched = 0

    for filing_id, cik, accession in rows:
        accession_nd = accession.replace("-", "")
        cik_clean = cik.lstrip("0") or "0"
        index_url = f"https://www.sec.gov/Archives/edgar/data/{cik_clean}/{accession_nd}/"

        index_html = fetch_sec_text(index_url)
        if not index_html:
            time.sleep(0.12)
            continue

        # Find primary 8-K HTML document (skip index, R1, exhibits)
        doc_href = None
        for part in index_html.split('href="')[1:]:
            href = part.split('"')[0]
            h = href.lower()
            fname = h.rsplit("/", 1)[-1] if "/" in h else h
            if not (fname.endswith(".htm") or fname.endswith(".html")):
                continue
            # Skip non-filing documents
            if any(skip in fname for skip in ["index", "r1.", "r2.", "r3.", "r4.", "ex-", "exhibit",
                                                "report.", "show.", "filingsummary"]):
                continue
            # Skip SEC navigation pages
            if fname.startswith("0") and "index" in fname:
                continue
            doc_href = href
            break

        if not doc_href:
            continue

        time.sleep(0.12)
        if doc_href.startswith("/"):
            doc_url = f"https://www.sec.gov{doc_href}"
        elif doc_href.startswith("http"):
            doc_url = doc_href
        else:
            doc_url = f"https://www.sec.gov/Archives/edgar/data/{cik_clean}/{accession_nd}/{doc_href}"
        body = fetch_sec_text(doc_url)
        if not body:
            continue

        # Extract Item numbers
        items = extract_8k_items(body)
        classification, severity = classify_8k(items)

        conn.execute(
            """UPDATE filings SET item_numbers=?, event_classification=?, event_severity=?
               WHERE id=?""",
            (json.dumps(items), classification, severity, filing_id)
        )
        enriched += 1

        if enriched % 50 == 0:
            conn.commit()
            print(f"  Enriched {enriched}/{len(rows)} 8-Ks")

        time.sleep(0.12)

    conn.commit()
    print(f"  8-K enrichment: {enriched} classified")
    return enriched


# --- Finnhub Price Downloads ---

def download_prices(conn: sqlite3.Connection, tickers: list[str], days: int = 730):
    """Download daily candles from Yahoo Finance (free, no API key needed)."""
    print(f"\nDownloading {days} days of prices for {len(tickers)} tickers via Yahoo Finance...")

    end_ts = int(datetime.now().timestamp())
    start_ts = int((datetime.now() - timedelta(days=days)).timestamp())

    total = 0
    errors = 0
    skipped = 0

    for i, ticker in enumerate(tickers):
        # Skip preferred shares and weird tickers
        if "-" in ticker or len(ticker) > 5:
            skipped += 1
            continue

        # Skip if we already have recent data
        latest = conn.execute(
            "SELECT MAX(date) FROM daily_prices WHERE ticker = ?", (ticker,)
        ).fetchone()[0]
        if latest and latest >= (datetime.now() - timedelta(days=3)).strftime("%Y-%m-%d"):
            skipped += 1
            continue

        # Yahoo Finance v8 chart API (free, no auth)
        url = f"https://query1.finance.yahoo.com/v8/finance/chart/{ticker}?period1={start_ts}&period2={end_ts}&interval=1d"
        try:
            req = urllib.request.Request(url)
            req.add_header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")
            with urllib.request.urlopen(req, timeout=15) as resp:
                data = json.loads(resp.read().decode())
        except Exception as e:
            errors += 1
            if errors <= 3 or errors % 20 == 0:
                print(f"  No data for {ticker}: {e}")
            if errors > 80:
                print(f"  Too many price errors ({errors}), stopping")
                break
            time.sleep(0.5)
            continue

        # Parse v8 chart response
        result = data.get("chart", {}).get("result", [])
        if not result:
            errors += 1
            continue

        r = result[0]
        timestamps = r.get("timestamp", [])
        quote = r.get("indicators", {}).get("quote", [{}])[0]
        closes = quote.get("close", [])
        opens = quote.get("open", [])
        highs = quote.get("high", [])
        lows = quote.get("low", [])
        volumes = quote.get("volume", [])

        if not timestamps or not closes:
            continue

        prev_close = None
        candles = 0
        for j in range(len(timestamps)):
            if j >= len(closes) or closes[j] is None:
                continue

            date_str = datetime.fromtimestamp(timestamps[j]).strftime("%Y-%m-%d")
            c = closes[j]
            o = opens[j] if j < len(opens) else None
            h = highs[j] if j < len(highs) else None
            l = lows[j] if j < len(lows) else None
            vol = volumes[j] if j < len(volumes) else None

            if c <= 0:
                continue

            change_1d = None
            if prev_close and prev_close > 0:
                change_1d = ((c - prev_close) / prev_close) * 100
            prev_close = c

            conn.execute(
                """INSERT OR IGNORE INTO daily_prices (ticker, date, open, high, low, close, volume, change_1d)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
                (ticker, date_str, o, h, l, c, vol, change_1d)
            )
            candles += 1

        total += candles

        if (i + 1) % 10 == 0:
            conn.commit()
            print(f"  Prices: {i+1}/{len(tickers)} tickers ({total} candles, {skipped} skipped, {errors} errors)")

        time.sleep(0.3)  # Be polite to Yahoo

    conn.commit()
    fetched_tickers = conn.execute("SELECT COUNT(DISTINCT ticker) FROM daily_prices").fetchone()[0]
    print(f"  Prices: {total} candles for {fetched_tickers} tickers ({errors} errors, {skipped} skipped)")
    return total


# --- Helper Functions ---

def extract_xml_value(xml: str, tag: str) -> Optional[str]:
    open_tag = f"<{tag}>"
    close_tag = f"</{tag}>"
    start = xml.find(open_tag)
    if start == -1:
        return None
    start += len(open_tag)
    end = xml.find(close_tag, start)
    if end == -1:
        return None
    return xml[start:end].strip()


def extract_nested_value(xml: str, parent_tag: str) -> Optional[str]:
    """Extract <value>X</value> from within a parent element, scoped to that element only."""
    open_tag = f"<{parent_tag}>"
    close_tag = f"</{parent_tag}>"
    start = xml.find(open_tag)
    if start == -1:
        return None
    end = xml.find(close_tag, start)
    if end == -1:
        # Fall back to a small window (200 chars) to avoid grabbing wrong values
        section = xml[start:start + 200]
    else:
        section = xml[start:end]
    val_start = section.find("<value>")
    if val_start == -1:
        return None
    val_start += 7
    val_end = section.find("</value>", val_start)
    if val_end == -1:
        return None
    val = section[val_start:val_end].strip()
    # Sanity check: should be numeric for transaction fields
    return val


def classify_form4(code, is_officer, is_director, total_value, shares, post_shares):
    if code == "P":
        if is_officer and total_value >= 100_000:
            return "strong_buy", 1.0
        elif is_officer and total_value >= 25_000:
            return "moderate_buy", 0.7
        elif is_director and total_value >= 50_000:
            return "moderate_buy", 0.6
        elif total_value >= 10_000:
            return "small_buy", 0.3
        else:
            return "minimal_buy", 0.1
    elif code == "S":
        if is_officer and post_shares > 0 and shares > 0:
            pct_sold = shares / (post_shares + shares)
            if pct_sold > 0.20:
                return "informative_sale", -0.3
        return "routine_sale", 0.0
    elif code == "A":
        return "award", 0.0
    elif code == "M":
        return "option_exercise", 0.0
    elif code == "G":
        return "gift", 0.0
    elif code == "F":
        return "tax_withholding", 0.0
    return "unknown", 0.0


def extract_8k_items(html: str) -> list[str]:
    import re
    text = re.sub(r'<[^>]+>', ' ', html).lower()
    items = []
    seen = set()
    for match in re.finditer(r'item\s+(\d\.\d{2})', text):
        item = match.group(1)
        if item not in seen:
            seen.add(item)
            items.append(item)
    return items


def classify_8k(items: list[str]) -> tuple[str, float]:
    mapping = {
        "2.01": ("acquisition_completion", 0.9),
        "2.02": ("earnings_results", 0.85),
        "1.01": ("material_agreement", 0.8),
        "7.01": ("regulation_fd", 0.7),
        "2.05": ("restructuring", -0.6),
        "2.06": ("material_impairment", -0.8),
        "1.02": ("agreement_termination", -0.5),
        "3.01": ("delisting_notice", -0.9),
        "4.01": ("auditor_change", -0.4),
        "5.02": ("executive_change", 0.5),
        "5.01": ("corporate_governance", 0.3),
        "5.07": ("shareholder_vote", 0.4),
        "8.01": ("other_event", 0.3),
    }
    for item in items:
        if item in mapping:
            return mapping[item]
    return "routine_filing", 0.1


# --- Main ---

def main():
    parser = argparse.ArgumentParser(description="Download historical SEC + price data")
    parser.add_argument("--db", default=DB_PATH, help="Database path")
    parser.add_argument("--tickers", help="Comma-separated tickers (e.g., AAPL,MSFT,NVDA)")
    parser.add_argument("--top", type=int, default=100, help="Top N companies from SEC")
    parser.add_argument("--days", type=int, default=730, help="Days of history")
    parser.add_argument("--prices-only", action="store_true", help="Only download prices")
    parser.add_argument("--enrich-only", action="store_true", help="Only enrich existing filings")
    parser.add_argument("--max-filings", type=int, default=3000, help="Max filings per form type")
    parser.add_argument("--max-enrich", type=int, default=1000, help="Max filings to enrich with XML")
    args = parser.parse_args()

    conn = init_db(args.db)
    print(f"Database: {args.db}")

    end_date = datetime.now().strftime("%Y-%m-%d")
    start_date = (datetime.now() - timedelta(days=args.days)).strftime("%Y-%m-%d")

    # Load companies
    if args.tickers:
        ticker_list = [t.strip().upper() for t in args.tickers.split(",")]
        # Look up CIKs
        all_companies = load_sp500_tickers(top_n=15000)
        companies = [(c, t, n) for c, t, n in all_companies if t in ticker_list]
        if not companies:
            print(f"No CIKs found for tickers: {args.tickers}")
            sys.exit(1)
    else:
        companies = load_sp500_tickers(top_n=args.top)

    # Store companies
    for cik, ticker, name in companies:
        conn.execute(
            "INSERT OR IGNORE INTO companies (cik, ticker, name) VALUES (?, ?, ?)",
            (cik, ticker, name)
        )
    conn.commit()
    print(f"Tracking {len(companies)} companies")

    ticker_list = [t for _, t, _ in companies]

    if not args.prices_only and not args.enrich_only:
        # Download filings
        download_filings(conn, "4", start_date, end_date, max_total=args.max_filings)
        time.sleep(1)
        download_filings(conn, "8-K", start_date, end_date, max_total=args.max_filings)

    if not args.prices_only:
        # Enrich with XML parsing
        enrich_form4_filings(conn, limit=args.max_enrich)
        enrich_8k_filings(conn, limit=args.max_enrich)

    # Download prices
    download_prices(conn, ticker_list, days=args.days)

    # Summary
    form4_count = conn.execute("SELECT COUNT(*) FROM filings WHERE form_type='4'").fetchone()[0]
    form4_enriched = conn.execute("SELECT COUNT(*) FROM filings WHERE form_type='4' AND transaction_code IS NOT NULL").fetchone()[0]
    eight_k_count = conn.execute("SELECT COUNT(*) FROM filings WHERE form_type='8-K'").fetchone()[0]
    eight_k_classified = conn.execute("SELECT COUNT(*) FROM filings WHERE form_type='8-K' AND event_classification IS NOT NULL").fetchone()[0]
    price_count = conn.execute("SELECT COUNT(*) FROM daily_prices").fetchone()[0]
    ticker_count = conn.execute("SELECT COUNT(DISTINCT ticker) FROM daily_prices").fetchone()[0]

    print(f"\n{'='*50}")
    print(f"BOOTSTRAP SUMMARY")
    print(f"{'='*50}")
    print(f"Form 4 filings:   {form4_count:>6} ({form4_enriched} enriched with XML)")
    print(f"8-K filings:      {eight_k_count:>6} ({eight_k_classified} classified)")
    print(f"Price candles:     {price_count:>6} ({ticker_count} tickers)")
    print(f"Database:          {args.db}")
    print(f"{'='*50}")

    conn.close()


if __name__ == "__main__":
    main()
