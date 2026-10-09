# Load the downloaded bars into wide date x symbol arrays, keeping symbols that were ever liquid.
import glob, os, numpy as np, pandas as pd
D = os.path.dirname(os.path.abspath(__file__))
def build():
    parts = []
    for p in sorted(glob.glob(f"{D}/bars/*.parquet")):
        b = pd.read_parquet(p)
        if b.empty: continue
        b["dv"] = b.c * b.v
        keep = b.groupby("sym").dv.apply(lambda s: s.rolling(20, min_periods=10).mean().max()) >= 10e6
        parts.append(b[b.sym.isin(keep[keep].index)].drop(columns="dv"))
    df = pd.concat(parts)
    df["date"] = pd.to_datetime(df.date)
    out = {k: df.pivot(index="date", columns="sym", values=k).astype("float32") for k in "ohlcv"}
    pd.to_pickle(out, f"{D}/panel.pkl")
    return out
def load():
    return pd.read_pickle(f"{D}/panel.pkl")
if __name__ == "__main__":
    P = build(); print(P["c"].shape, P["c"].index.min(), P["c"].index.max())
