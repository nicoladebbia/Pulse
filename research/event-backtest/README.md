# Event-signal backtest (2026-10-09)

Can any researched event signal give the bot a proven edge on stocks it can actually trade? First round: **no**. Second round (below): outside director buys, with long holds and loose stops.

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

## Second round: making them usable (2026-10-09)
Benchmark changed to an equal-weight basket of liquid stocks in the same dollar-volume quintile (`engine.SizeBench`): SPY made every small and mid cap look bad in 2023-25. Train 2018-22, test 2023-26 for prices and insiders; 2023-24 vs 2025-26 for news. t-stats clustered by month (insiders) or day (news).

| Signal | Result | In the bot |
|---|---|---|
| **Outside director buys** (director, not officer, not 10% owner, $10k+, filed within 10 days, liquid), 60-day hold | +1.18% vs size peers, t 2.5, no stops. 6x ATR trail + 30% floor: +0.53% (t 2.2; 1.9 in 2018-22, 1.1 in 2023-26), ~4 a day. The bot's 3x ATR / 15% exits: ~0. ATR < 3% names did better (+0.90%) than ATR >= 3% (+0.38%). $50k+ buys were weaker. | `insider_director_buy`: 60 trading days, `ExitRules::LOOSE`, no ATR floor, sized to its 20-30% stop. |
| Insider clusters, CEO/CFO buys, 10% owners | No edge. | `insider_cluster` no longer detected. |
| News (both directions): next-day entry, conditioned on the price reaction, buying the bad-news dip, shorting the good-news jump, 10:00 entry on the news morning (`news2`, `dip`, `fast_news`, `get_10am`) | Nothing beats the 0.20% cost in both periods; most lose about the cost. | Recorded and scored, not traded (`NEWS_TRADES_ENABLED=true` turns it back on). |

The director edge is weak: about half a percent per 60-day trade, and not significant in 2023-26 alone. It trades at half the smallest size so the paper account and the learning report can confirm it.

Scripts: `insiders2.py` (role flags, filing lag), `insider_grid.py`, `insider_combo.py`, `director_exits.py`, `director_atr.py`, `news2.py`, `dip.py`, `get_10am.py` + `fast_news.py`.

Run order:
1. `get_bars.py`, `get_news.py` (needs `ALPACA_*`), `insiders.py` (download the SEC zips into `f345/` first), `panel.py`.
2. Then the test scripts.

Data files are not committed.
