//! Minimal Anthropic Messages + Message Batches client for the research lane.
//!
//! Separate from `claude::client::call_anthropic` on purpose: that one takes a
//! plain string and knows nothing about content blocks,
//! structured outputs or batches — all three are needed to read a whole paper.
//! Raw reqwest because there is no official Rust SDK.

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://api.anthropic.com/v1";
const VERSION: &str = "2023-06-01";

pub const TRIAGE_MODEL: &str = "claude-haiku-4-5";
pub const READ_MODEL: &str = "claude-opus-5";

/// Batch API price multiplier (50% off, both directions).
pub const BATCH_DISCOUNT: f64 = 0.5;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct Reply {
    /// Concatenated text blocks (thinking blocks dropped).
    pub text: String,
    pub stop_reason: String,
    pub usage: Usage,
}

pub fn api_key() -> anyhow::Result<String> {
    // Paper reads need PDF blocks, structured outputs and the Batch API, none of
    // which a local model offers, so the research lane stays cloud-only.
    if pulse_llm::is_local() {
        bail!("research lane needs the Anthropic API and is off in local mode (PULSE_LLM=local)");
    }
    std::env::var("ANTHROPIC_API_KEY").map_err(|_| anyhow!("ANTHROPIC_API_KEY not set"))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        // Every synchronous call here is short (Haiku triage, count_tokens, batch
        // submit/poll/results); deep reads go through the Batch API. With 3
        // attempts this bounds a dead network at ~6 minutes, not 45.
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_default()
}

/// Request params for one structured-output call. `content` is the user turn's
/// content block array (text and/or document blocks).
pub fn params(model: &str, system: &str, content: Value, schema: &Value, max_tokens: u32) -> Value {
    let mut p = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": content }],
        "output_config": { "format": { "type": "json_schema", "schema": schema } },
    });
    // Haiku 4.5 rejects `effort` and uses the old thinking config; triage needs neither.
    if !model.contains("haiku") {
        p["thinking"] = json!({ "type": "adaptive" });
        p["output_config"]["effort"] = json!("high");
    }
    p
}

/// Parse a Messages API response body (also the `message` inside a batch result).
pub fn parse_reply(v: &Value) -> anyhow::Result<Reply> {
    let stop_reason = v["stop_reason"].as_str().unwrap_or("").to_string();
    let text: String = v["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let usage = Usage {
        input_tokens: v["usage"]["input_tokens"].as_i64().unwrap_or(0)
            + v["usage"]["cache_read_input_tokens"].as_i64().unwrap_or(0)
            + v["usage"]["cache_creation_input_tokens"].as_i64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_i64().unwrap_or(0),
    };
    Ok(Reply { text, stop_reason, usage })
}

/// Structured output is only trustworthy on a clean stop: `max_tokens` truncates
/// the JSON and `refusal` may not follow the schema at all.
pub fn json_of(reply: &Reply) -> anyhow::Result<Value> {
    match reply.stop_reason.as_str() {
        "end_turn" => serde_json::from_str(&reply.text).context("structured output did not parse"),
        other => bail!("unusable stop_reason '{other}' ({} output tokens)", reply.usage.output_tokens),
    }
}

/// `idempotent = false` (batch create): a transport error is NOT retried, since the
/// server may have created the batch before the connection dropped, and a second
/// POST would create, and bill, a second batch nobody tracks.
async fn send_with_retry(idempotent: bool, req: impl Fn() -> reqwest::RequestBuilder) -> anyhow::Result<Value> {
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(15 * attempt)).await;
        }
        match req().send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    return serde_json::from_str(&body).context("Anthropic response is not JSON");
                }
                last = format!("{status}: {}", body.chars().take(300).collect::<String>());
                // 429 / 5xx / 529 overloaded are transient; any other 4xx is our bug.
                if !(status.as_u16() == 429 || status.is_server_error()) {
                    bail!("Anthropic API {last}");
                }
            }
            Err(e) if idempotent => last = e.to_string(),
            Err(e) => bail!("Anthropic API transport error (not retried: may have been accepted): {e}"),
        }
    }
    bail!("Anthropic API failed after 3 attempts: {last}")
}

fn post(client: &reqwest::Client, key: &str, path: &str, body: &Value) -> reqwest::RequestBuilder {
    client
        .post(format!("{API}{path}"))
        .header("x-api-key", key)
        .header("anthropic-version", VERSION)
        .json(body)
}

fn get(client: &reqwest::Client, key: &str, url: &str) -> reqwest::RequestBuilder {
    client.get(url).header("x-api-key", key).header("anthropic-version", VERSION)
}

/// One synchronous call.
pub async fn create(key: &str, params: &Value) -> anyhow::Result<Reply> {
    let client = http();
    let v = send_with_retry(true, || post(&client, key, "/messages", params)).await?;
    parse_reply(&v)
}

/// Exact input-token count for `params` (free endpoint). Used to price a read
/// BEFORE it is submitted, so the spend cap is enforced on real numbers.
pub async fn count_tokens(key: &str, params: &Value) -> anyhow::Result<i64> {
    let mut body = json!({
        "model": params["model"],
        "system": params["system"],
        "messages": params["messages"],
    });
    if let Some(t) = params.get("thinking") {
        body["thinking"] = t.clone();
    }
    let client = http();
    let v = send_with_retry(true, || post(&client, key, "/messages/count_tokens", &body)).await?;
    v["input_tokens"].as_i64().ok_or_else(|| anyhow!("count_tokens: no input_tokens in {v}"))
}

/// Submit a batch. `requests` = (custom_id, params). Returns the batch id.
pub async fn submit_batch(key: &str, requests: &[(String, Value)]) -> anyhow::Result<String> {
    let body = json!({
        "requests": requests
            .iter()
            .map(|(id, p)| json!({ "custom_id": id, "params": p }))
            .collect::<Vec<_>>()
    });
    let client = http();
    let v = send_with_retry(false, || post(&client, key, "/messages/batches", &body)).await?;
    v["id"].as_str().map(str::to_string).ok_or_else(|| anyhow!("batch create: no id in response"))
}

pub enum BatchState {
    InProgress,
    /// Results URL.
    Ended(String),
}

pub async fn batch_state(key: &str, batch_id: &str) -> anyhow::Result<BatchState> {
    let client = http();
    let url = format!("{API}/messages/batches/{batch_id}");
    let v = send_with_retry(true, || get(&client, key, &url)).await?;
    match v["processing_status"].as_str() {
        Some("ended") => Ok(BatchState::Ended(
            v["results_url"].as_str().ok_or_else(|| anyhow!("ended batch without results_url"))?.to_string(),
        )),
        Some(_) => Ok(BatchState::InProgress),
        None => bail!("batch {batch_id}: no processing_status"),
    }
}

/// (custom_id, Ok(reply) | Err(reason)). Keyed by custom_id — results arrive in
/// any order.
pub fn parse_batch_results(jsonl: &str) -> Vec<(String, Result<Reply, String>)> {
    jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let id = v["custom_id"].as_str()?.to_string();
            let r = &v["result"];
            let out = match r["type"].as_str() {
                Some("succeeded") => parse_reply(&r["message"]).map_err(|e| e.to_string()),
                Some(other) => Err(format!("batch result {other}: {}", r["error"])),
                None => Err("batch result without type".to_string()),
            };
            Some((id, out))
        })
        .collect()
}

pub async fn batch_results(key: &str, results_url: &str) -> anyhow::Result<Vec<(String, Result<Reply, String>)>> {
    let client = http();
    let resp = get(&client, key, results_url).send().await.context("batch results request failed")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("batch results {status}: {}", body.chars().take(300).collect::<String>());
    }
    Ok(parse_batch_results(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haiku_params_carry_no_effort_or_thinking() {
        let s = json!({"type":"object"});
        let h = params(TRIAGE_MODEL, "sys", json!([{"type":"text","text":"x"}]), &s, 100);
        assert!(h.get("thinking").is_none());
        assert!(h["output_config"].get("effort").is_none());
        assert_eq!(h["output_config"]["format"]["type"], "json_schema");
        let o = params(READ_MODEL, "sys", json!([]), &s, 100);
        assert_eq!(o["thinking"]["type"], "adaptive");
        assert_eq!(o["output_config"]["effort"], "high");
    }

    #[test]
    fn parses_reply_and_rejects_truncation() {
        let v = json!({
            "stop_reason": "end_turn",
            "content": [{"type":"thinking","thinking":""},{"type":"text","text":"{\"a\":1}"}],
            "usage": {"input_tokens": 10, "cache_read_input_tokens": 5, "output_tokens": 7}
        });
        let r = parse_reply(&v).unwrap();
        assert_eq!(r.usage, Usage { input_tokens: 15, output_tokens: 7 });
        assert_eq!(json_of(&r).unwrap()["a"], 1);

        let mut cut = r.clone();
        cut.stop_reason = "max_tokens".into();
        assert!(json_of(&cut).is_err());
        cut.stop_reason = "refusal".into();
        assert!(json_of(&cut).is_err());
    }

    #[test]
    fn batch_results_keyed_by_custom_id_in_any_order() {
        let jsonl = r#"{"custom_id":"paper-9","result":{"type":"errored","error":{"type":"overloaded_error"}}}
{"custom_id":"paper-3","result":{"type":"succeeded","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"{}"}],"usage":{"input_tokens":1,"output_tokens":2}}}}
{"custom_id":"paper-4","result":{"type":"expired"}}
"#;
        let r = parse_batch_results(jsonl);
        assert_eq!(r.len(), 3);
        let get = |id: &str| r.iter().find(|(c, _)| c == id).map(|(_, v)| v.clone()).unwrap();
        assert!(get("paper-3").is_ok());
        assert!(get("paper-9").unwrap_err().contains("errored"));
        assert!(get("paper-4").unwrap_err().contains("expired"));
    }
}
