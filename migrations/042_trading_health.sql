-- A sell that does not fill within the five-second poll (pre-market, or a
-- slow fill) is remembered on its trade, so the next run books it with the
-- reason it was placed for instead of selling again or calling it
-- 'closed_between_runs'. TENB was half-closed twice this way (2026-10-06/07).
ALTER TABLE paper_trades ADD COLUMN exit_order_id TEXT;
ALTER TABLE paper_trades ADD COLUMN exit_order_reason TEXT;
ALTER TABLE paper_trades ADD COLUMN exit_order_kind TEXT;

-- Universe verdicts cached while Finnhub or Alpaca was failing (no price, no
-- Alpaca status) are dropped so those tickers are checked again.
DELETE FROM ticker_eligibility_cache
WHERE eligible = 0 AND (last_price IS NULL OR market_cap_millions IS NULL OR COALESCE(alpaca_status, '') = '');

-- News momentum counts news-story mentions only. The activity windows count
-- every mention (filings, Wikipedia pageviews), so a pageview spike also
-- scored as news.
ALTER TABLE signals ADD COLUMN news_window_7d INTEGER NOT NULL DEFAULT 0;
ALTER TABLE signals ADD COLUMN news_acceleration REAL NOT NULL DEFAULT 0;
