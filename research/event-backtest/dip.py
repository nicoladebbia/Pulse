# "Bad news overreaction": buy a stock that fell hard on a downgrade / miss / guidance cut / price-target cut.
import os, sys, numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from panel import load
from engine import Panel, SizeBench, trades_vs, sim_vs
D = os.path.dirname(os.path.abspath(__file__))
X = Panel(load()); B = SizeBench(X)
BAD = {"downgrade": r"\bDowngrades\b.+\bto\b", "miss_both": r"EPS .*Misses.*Sales .*Miss",
       "guide_down": r"(Lowers|Cuts|Reduces) (FY|Q\d|\d{4}|Full.Year|Annual|Its |Outlook|Guidance).{0,60}(Guidance|Outlook|Forecast|Sales|Revenue|EPS)|Sees .{0,60}Below .{0,20}Est",
       "pt_lower": r"Maintains .+ Lowers Price Target"}
GOOD = {"upgrade": r"\bUpgrades\b.+\bto\b", "beat_both": r"EPS .*Beats.*Sales .*Beat",
        "guide_up": r"Raises (FY|Q\d|\d{4}|Full.Year|Annual|Its |Outlook|Guidance).{0,60}(Guidance|Outlook|Forecast|Sales|Revenue|EPS)|Sees .{0,60}Above .{0,20}Est",
        "pt_raise": r"Maintains .+ Raises Price Target"}
def tag(n, rules):
    k = pd.Series("", index=n.index)
    for name, pat in rules.items():
        k = k.mask((k == "") & n.headline.str.contains(pat, regex=True).values, name)
    return k
def stats(t, periods=((2023, 2024), (2025, 2026))):
    out = []
    for lo, hi in periods:
        u = t[(t.date.dt.year >= lo) & (t.date.dt.year <= hi)]
        if len(u) < 30: out.append(f"{lo}-{hi%100}: n{len(u)}"); continue
        d = u.groupby("date").excess.mean(); tt = d.mean() / (d.std() / np.sqrt(len(d)))
        out.append(f"{lo}-{hi%100}: {u.excess.mean()*100:+.2f}% med {u.excess.median()*100:+.2f}% t{tt:+.1f} n{len(u)}")
    return " | ".join(out)
def events(n, rules, lo, hi, side=1):
    k = tag(n, rules); m = n[(k != "") & ~n.headline.str.contains("Top Ratings|Benzinga")].assign(kind=k)
    m = m[(m.react <= hi) & (m.react > lo)] if side > 0 else m[(m.react >= lo) & (m.react < hi)]
    return m.assign(date=m.news_day, sym=m.symbols, side=side)[["date", "sym", "side", "kind"]].drop_duplicates(["date", "sym"])
if __name__ == "__main__":
    n = pd.read_parquet(f"{D}/news_events.parquet").reset_index(drop=True)
    for name, rules in [("any bad", BAD)] + [(k, {k: v}) for k, v in BAD.items()]:
        for lo, hi in ((-0.50, -0.03), (-0.50, -0.05), (-0.50, -0.08), (-0.20, -0.05)):
            ev = events(n, rules, lo, hi)
            print(f"BUY {name:10s} drop {hi:+.0%}..{lo:+.0%} " + " || ".join(f"h{h}: {stats(trades_vs(X, B, ev, h))}" for h in (5, 10, 20)), flush=True)
    ev = events(n, BAD, -0.50, -0.05)
    for mult, hard, mh in ((3, .15, 10), (3, .15, 20), (5, .25, 20)):
        print(f"SIM any bad drop>5% trail={mult} hard={hard} hold={mh}: {stats(sim_vs(X, B, ev, mh, mult, hard))}", flush=True)
    # Mirror: short good news that already jumped (overreaction up)?
    ev = events(n, GOOD, 0.05, 0.50, side=-1)
    print("SHORT good news jump>5% " + " || ".join(f"h{h}: {stats(trades_vs(X, B, ev, h))}" for h in (5, 10, 20)))
