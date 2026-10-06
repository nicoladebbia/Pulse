-- Data-source fetches log one row per provider per run with the number of
-- HTTP requests in `calls`, instead of one row per request: the 5-minute
-- live-signals run makes ~300 SEC requests each time. Counts read SUM(calls).
ALTER TABLE api_usage ADD COLUMN calls INTEGER NOT NULL DEFAULT 1;
