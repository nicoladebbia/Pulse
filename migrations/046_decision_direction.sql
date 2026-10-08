-- Short candidates (event signals sold short) are logged in trade_decisions
-- too. The learning report scores each filter on what its signals did next,
-- so it must know which way a candidate pointed.
ALTER TABLE trade_decisions ADD COLUMN direction TEXT NOT NULL DEFAULT 'long';
