# Richer insider purchase table: shares owned after, ownership change, role flags.
import zipfile, glob, os, pandas as pd, numpy as np
D = os.path.dirname(os.path.abspath(__file__)); out = []
for z in sorted(glob.glob(f"{D}/f345/*.zip")):
    with zipfile.ZipFile(z) as f:
        rd = lambda n, cols: pd.read_csv(f.open(n), sep="\t", usecols=cols, dtype=str, on_bad_lines="skip")
        sub = rd("SUBMISSION.tsv", ["ACCESSION_NUMBER", "FILING_DATE", "DOCUMENT_TYPE", "ISSUERTRADINGSYMBOL", "ISSUERCIK"])
        own = rd("REPORTINGOWNER.tsv", ["ACCESSION_NUMBER", "RPTOWNERCIK", "RPTOWNER_RELATIONSHIP", "RPTOWNER_TITLE"])
        tr = rd("NONDERIV_TRANS.tsv", ["ACCESSION_NUMBER", "TRANS_CODE", "TRANS_DATE", "TRANS_SHARES", "TRANS_PRICEPERSHARE", "TRANS_ACQUIRED_DISP_CD", "SHRS_OWND_FOLWNG_TRANS", "DIRECT_INDIRECT_OWNERSHIP"])
    tr = tr[(tr.TRANS_CODE == "P") & (tr.TRANS_ACQUIRED_DISP_CD == "A")].copy()
    for c in ("TRANS_SHARES", "TRANS_PRICEPERSHARE", "SHRS_OWND_FOLWNG_TRANS"): tr[c] = pd.to_numeric(tr[c], errors="coerce")
    tr["usd"] = tr.TRANS_SHARES * tr.TRANS_PRICEPERSHARE
    g = tr.groupby("ACCESSION_NUMBER")
    agg = pd.DataFrame({"usd": g.usd.sum(), "shares": g.TRANS_SHARES.sum(), "trans_date": g.TRANS_DATE.min(),
                        "owned_after": g.SHRS_OWND_FOLWNG_TRANS.max(), "direct": g.DIRECT_INDIRECT_OWNERSHIP.apply(lambda s: (s == "D").any())}).reset_index()
    m = agg.merge(sub[sub.DOCUMENT_TYPE.isin(["4", "4/A"])], on="ACCESSION_NUMBER").merge(own.drop_duplicates("ACCESSION_NUMBER"), on="ACCESSION_NUMBER", how="left")
    out.append(m)
df = pd.concat(out)
df["filed"] = pd.to_datetime(df.FILING_DATE, format="%d-%b-%Y", errors="coerce")
df["traded"] = pd.to_datetime(df.trans_date, format="%d-%b-%Y", errors="coerce")
df["sym"] = df.ISSUERTRADINGSYMBOL.str.upper().str.strip()
df = df.dropna(subset=["filed", "sym", "usd"]).drop_duplicates("ACCESSION_NUMBER")
before = (df.owned_after - df.shares).clip(lower=0)
df["own_chg"] = np.where(before > 0, df.shares / before, np.inf)          # fraction added to the stake
rel = df.RPTOWNER_RELATIONSHIP.fillna(""); title = df.RPTOWNER_TITLE.fillna("")
df["is_officer"] = rel.str.contains("Officer", case=False)
df["is_director"] = rel.str.contains("Director", case=False)
df["is_10pct"] = rel.str.contains("Ten", case=False)
df["is_ceo_cfo"] = title.str.contains(r"CEO|Chief Executive|CFO|Chief Financial", case=False, regex=True)
df["lag_days"] = (df.filed - df.traded).dt.days
df[["sym", "ISSUERCIK", "filed", "traded", "usd", "shares", "owned_after", "own_chg", "direct", "RPTOWNERCIK", "is_officer", "is_director", "is_10pct", "is_ceo_cfo", "lag_days"]].to_parquet(f"{D}/insider_buys2.parquet")
print(len(df), df.own_chg.replace(np.inf, np.nan).describe().round(3).to_dict())
