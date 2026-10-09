# Event-signal backtest (2026-10-09)

Can any researched event signal give the bot a proven edge on stocks it can actually trade? Short answer: **no, not yet**. No new event type was switched on.

## Setup
- **Universe**: every NYSE/Nasdaq/Arca/AMEX symbol on Alpaca, including delisted ones, 2018-01 to 2026-10.
  - Daily SIP bars, split and dividend adjusted.
  - Point-in-time liquidity filter: price >= $5 and 20-day dollar volume >= $10M.
- **Trade timing**: the signal is known after the close of day t, entry is at the open of t+1, and exit is at the close of t+h.
- **Measurement**: return minus SPY over the same window, minus 0.20% round-trip cost. t-stats are clustered by day.
- **Shorts**: limited to names Alpaca marks shortable and easy to borrow today. This is the bot's own rule, but it is survivor-biased.
- **Bot exits** (`robust.sim`): 3x ATR trailing stop, ±15% hard stop, max hold.
- **News**: Benzinga headlines from the Alpaca News API, 2023-01 to 2026-10, single-ticker headlines classified by regex.
  - Aggressive timing: news before 9:30 ET trades at that day's open.
  - Conservative timing: the next day.
- **Insider buys**: SEC Form 3/4/5 data sets, code P purchases. "Routine" follows the Cohen-Malloy-Pomorski definition.

## Results
| Signal | Result |
|---|---|
| Insider buying: any purchase, opportunistic only, CEO/CFO $100k+, or 2+ insiders $100k+ in 10 days | ~0 or negative at 1-20 days (t between -2 and +1). The bot's `insider_cluster` has no edge. |
| Good news, long: upgrades, EPS+sales beats, guidance raised, price-target raises, buybacks | Negative after costs at 3-20 days in both 2023-24 and 2025-26. Several have t < -2. The move is already priced by the next open. |
| Bad news, short: downgrades, misses, guidance cut, equity offerings, price-target cuts | Mixed. Nothing is significant in both periods. |
| Short after an earnings beat (contrarian) | +0.2-0.8% at 10-20 days, but not significant in 2025-26. |
| Short after a big up-day on 3x volume | +4.8% per 20 days overall (t 11), but almost all of it comes from hard-to-borrow names. Easy-to-borrow: ~+1% with no stops; ~0 with the bot's stops; ~0 in 2018-22. |
| Short after a big down-day on volume (continuation) | +1.5-2% overall, ~0 on easy-to-borrow names. |
| Buy after a quiet big drop (reversal) | Negative: drops keep going. |

Run order:
1. `get_bars.py`, `get_news.py` (needs `ALPACA_*`), `insiders.py` (download the SEC zips into `f345/` first), `panel.py`.
2. Then the test scripts.

Data files are not committed.
