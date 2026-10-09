# Open-market insider purchases (Form 4, code P) from SEC's quarterly Form 3/4/5 data sets.
import zipfile, glob, os, pandas as pd
D = os.path.dirname(os.path.abspath(__file__)); out = []
for z in sorted(glob.glob(f"{D}/f345/*.zip")):
    with zipfile.ZipFile(z) as f:
        rd = lambda n, cols: pd.read_csv(f.open(n), sep="\t", usecols=cols, dtype=str, on_bad_lines="skip")
        sub = rd("SUBMISSION.tsv", ["ACCESSION_NUMBER", "FILING_DATE", "DOCUMENT_TYPE", "ISSUERTRADINGSYMBOL"])
        own = rd("REPORTINGOWNER.tsv", ["ACCESSION_NUMBER", "RPTOWNERCIK", "RPTOWNER_RELATIONSHIP", "RPTOWNER_TITLE"])
        tr = rd("NONDERIV_TRANS.tsv", ["ACCESSION_NUMBER", "TRANS_CODE", "TRANS_DATE", "TRANS_SHARES", "TRANS_PRICEPERSHARE", "TRANS_ACQUIRED_DISP_CD"])
    tr = tr[(tr.TRANS_CODE == "P") & (tr.TRANS_ACQUIRED_DISP_CD == "A")]
    tr = tr.assign(usd=pd.to_numeric(tr.TRANS_SHARES, errors="coerce") * pd.to_numeric(tr.TRANS_PRICEPERSHARE, errors="coerce"))
    tr = tr.groupby("ACCESSION_NUMBER", as_index=False).agg(usd=("usd", "sum"), trans_date=("TRANS_DATE", "min"))
    own = own.drop_duplicates("ACCESSION_NUMBER")
    m = tr.merge(sub[sub.DOCUMENT_TYPE.isin(["4", "4/A"])], on="ACCESSION_NUMBER").merge(own, on="ACCESSION_NUMBER", how="left")
    out.append(m); print(os.path.basename(z), len(m), flush=True)
df = pd.concat(out)
df["filed"] = pd.to_datetime(df.FILING_DATE, format="%d-%b-%Y", errors="coerce")
df["traded"] = pd.to_datetime(df.trans_date, format="%d-%b-%Y", errors="coerce")
df["sym"] = df.ISSUERTRADINGSYMBOL.str.upper().str.strip()
df = df.dropna(subset=["filed", "sym", "usd"]).drop_duplicates("ACCESSION_NUMBER")
df[["sym", "filed", "traded", "usd", "RPTOWNERCIK", "RPTOWNER_RELATIONSHIP", "RPTOWNER_TITLE"]].to_parquet(f"{D}/insider_buys.parquet")
print(len(df), "purchases", df.filed.min(), df.filed.max())
