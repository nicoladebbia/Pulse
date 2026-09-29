//! Full paper text for the deep read.
//!
//! Primary source: arXiv's LaTeXML HTML render. Measured 2026-09-24 on three
//! real q-fin papers, it costs 2.2x fewer tokens than sending the PDF (17k vs 38k,
//! 36k vs 70k, 59k vs 135k) and keeps every equation via `<math alttext>` — a deep
//! read that cannot see the model's equations is not a deep read. 17/20 recent
//! papers had an HTML render; the rest (404) fall back to the PDF as a document
//! block, which Claude reads natively (text + page images).

use anyhow::{bail, Context};
use regex::Regex;
use std::sync::LazyLock;
use std::time::Duration;

const HOST: &str = "https://export.arxiv.org";

pub enum PaperText {
    Html(String),
    /// Raw PDF bytes.
    Pdf(Vec<u8>),
}

static DROP_BLOCKS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<(script|style|nav|header|footer|button)\b[^>]*>.*?</(script|style|nav|header|footer|button)>").unwrap()
});
static MATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<math\b[^>]*?\balttext="([^"]*)"[^>]*>.*?</math>"#).unwrap());
static BIBLIOGRAPHY: LazyLock<Regex> =
    // Only the references section itself: appendices (proofs, robustness tables)
// often FOLLOW it — cutting to the end lost 414 of 659 equations in 2609.08581.
    LazyLock::new(|| Regex::new(r#"(?is)<section[^>]*class="ltx_bibliography"[^>]*>.*?</section>"#).unwrap());
static CELL_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</t[dh]>").unwrap());
static BLOCK_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)</(p|div|tr|li|h[1-6]|section|figcaption|table|caption)>|<br\s*/?>").unwrap());
static HEADING_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<h([1-6])\b[^>]*>").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\u{a0}]+").unwrap());
static BLANK_LINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n\s*(\n\s*)+").unwrap());

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// LaTeXML HTML → plain text that keeps what a reader needs: headings as
/// markdown, equations as `$TeX$`, table rows as `a | b | c` lines. The
/// bibliography is dropped (10-15% of tokens, no method content).
pub fn html_to_text(html: &str) -> String {
    let body = match html.find("<article") {
        Some(i) => &html[i..],
        None => html,
    };
    // Source newlines are insignificant in HTML; structure comes back from the
    // block/row/cell ends below. Without this, every table cell lands on its own line.
    let flat = body.replace(['\n', '\r'], " ");
    let s = DROP_BLOCKS.replace_all(&flat, "");
    let s = BIBLIOGRAPHY.replace_all(&s, "");
    let s = MATH.replace_all(&s, |c: &regex::Captures| format!(" ${}$ ", unescape(&c[1])));
    let s = HEADING_OPEN.replace_all(&s, |c: &regex::Captures| {
        let level: usize = c[1].parse().unwrap_or(2);
        format!("\n\n{} ", "#".repeat(level.min(4)))
    });
    let s = CELL_END.replace_all(&s, " | ");
    let s = BLOCK_END.replace_all(&s, "\n");
    let s = TAG.replace_all(&s, "");
    let s = unescape(&s);
    let s = SPACES.replace_all(&s, " ");
    let s: String = s.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    BLANK_LINES.replace_all(&s, "\n\n").trim().to_string()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("Pulse/1.0 (personal research reader; mailto:nicolagiovannidebbia@gmail.com)")
        .build()
        .unwrap_or_default()
}

/// Fetch the paper: HTML when arXiv has a render, else the PDF.
/// A PDF above 30 MB is refused (the request cap is 32 MB after base64 growth
/// would be exceeded well before that — base64 inflates by 4/3).
pub async fn fetch(arxiv_id: &str) -> anyhow::Result<PaperText> {
    let c = client();
    let resp = c.get(format!("{HOST}/html/{arxiv_id}")).send().await.context("arXiv HTML request failed")?;
    if resp.status().is_success() {
        let html = resp.text().await?;
        let text = html_to_text(&html);
        // A render that converted but lost the body is not a paper.
        if text.split_whitespace().count() >= 1500 {
            return Ok(PaperText::Html(text));
        }
        tracing::warn!("research: {arxiv_id} HTML render has <1500 words; using PDF");
    } else if resp.status().as_u16() != 404 {
        bail!("arXiv HTML {} for {arxiv_id}", resp.status());
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let resp = c.get(format!("{HOST}/pdf/{arxiv_id}")).send().await.context("arXiv PDF request failed")?;
    if !resp.status().is_success() {
        bail!("arXiv PDF {} for {arxiv_id}", resp.status());
    }
    let bytes = resp.bytes().await?.to_vec();
    if bytes.len() > 22 * 1024 * 1024 {
        bail!("PDF is {} MB; base64 would exceed the 32 MB request cap", bytes.len() / (1024 * 1024));
    }
    if !bytes.starts_with(b"%PDF") {
        bail!("arXiv returned a non-PDF body for {arxiv_id}");
    }
    Ok(PaperText::Pdf(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real LaTeXML markup, cut from arxiv.org/html/2608.02778 (2026-09-24).
    const SPECIMEN: &str = r#"<html><head><style>.x{}</style><script>var a=1;</script></head><body>
<nav class="ltx_page_navbar">Contents</nav>
<article class="ltx_document ltx_authors_1line">
<h1 class="ltx_title ltx_title_document">Neural Networks with Local Converging Inputs for Efficient Options Pricing Models</h1>
<p class="ltx_p">Let <math id="abstract1.m1" class="ltx_Math" alttext="S" display="inline" intent=":literal"><semantics><mi>S</mi><annotation encoding="application/x-tex">S</annotation></semantics></math> be the spot &amp; its drift.</p>
<table id="S4.T1.1" class="ltx_tabular ltx_centering ltx_guessed_headers ltx_align_middle">
<thead class="ltx_thead">
<tr id="S4.T1.1.1" class="ltx_tr">
<th id="S4.T1.1.1.1" class="ltx_td ltx_align_center ltx_th ltx_th_column ltx_th_row ltx_border_tt"><span id="S4.T1.1.1.1.1" class="ltx_text ltx_font_bold">Training Gap</span></th>
<th id="S4.T1.1.1.2" class="ltx_td ltx_align_center ltx_th ltx_th_column ltx_border_tt"><span id="S4.T1.1.1.2.1" class="ltx_text ltx_font_bold">Refined RMSE (Train)</span></th></tr>
</thead>
<tbody class="ltx_tbody">
<tr id="S4.T1.1.2" class="ltx_tr">
<th id="S4.T1.1.2.1" class="ltx_td ltx_align_center ltx_th ltx_th_row ltx_border_t">2</th>
<td id="S4.T1.1.2.2" class="ltx_td ltx_align_center ltx_border_t"><math id="S4.T1.m1" class="ltx_Math" alttext="2.119305\times 10^{0}" display="inline" intent=":literal"><semantics><mrow><mn>2.119305</mn><mo>×</mo><msup><mn>10</mn><mn>0</mn></msup></mrow><annotation encoding="application/x-tex">2.119305\times 10^{0}</annotation></semantics></math></td></tr>
</tbody></table>
<section id="bib" class="ltx_bibliography"><h2>References</h2><ul><li>Black, F. and Scholes, M. (1973)</li></ul></section>
<section id="A1" class="ltx_appendix"><h2>Appendix A Proof of Lemma 1</h2><p>By <math alttext="\epsilon" display="inline"><mi>e</mi></math>-argument.</p></section>
</article></body></html>"#;

    #[test]
    fn keeps_equations_tables_and_headings() {
        let t = html_to_text(SPECIMEN);
        assert!(t.starts_with("# Neural Networks with Local Converging Inputs"), "{t}");
        assert!(t.contains("Let $S$ be the spot & its drift."), "{t}");
        // TeX appears once: the <annotation> duplicate is consumed with the <math>.
        assert_eq!(t.matches(r"2.119305\times 10^{0}").count(), 1, "{t}");
        assert!(t.contains("Training Gap | Refined RMSE (Train) |"), "{t}");
        assert!(t.contains(r"2 | $2.119305\times 10^{0}$ |"), "{t}");
        // Appendices after the references survive (they hold proofs and robustness).
        assert!(t.contains(r"By $\epsilon$ -argument."), "{t}");
    }

    #[test]
    fn drops_chrome_and_bibliography() {
        let t = html_to_text(SPECIMEN);
        for gone in ["var a", ".x{}", "Contents", "Black, F.", "References"] {
            assert!(!t.contains(gone), "'{gone}' leaked into: {t}");
        }
        assert!(!t.contains('<'), "{t}");
        assert!(!t.contains("\n\n\n"), "{t}");
    }
}

