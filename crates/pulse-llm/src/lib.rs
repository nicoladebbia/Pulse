//! Where Pulse's AI calls go: the cloud APIs (default) or a local Ollama.
//!
//! `PULSE_LLM=local` sends every call to Ollama on this machine instead of
//! Anthropic, Groq and Voyage, so Pulse runs with no API keys and no cost.
//! Ollama speaks all three wire formats (Anthropic `/v1/messages`, OpenAI
//! `/v1/chat/completions` and `/v1/embeddings`), so call sites keep their
//! request shapes and only ask this crate for the URL, key and body.
//!
//! Settings (all optional except `PULSE_LLM`):
//!   PULSE_LLM=local                 turn local mode on
//!   PULSE_LOCAL_MODEL               chat model   (default pulse-local, see ollama/Modelfile)
//!   PULSE_LOCAL_EMBED_MODEL         embedding model (default embeddinggemma)
//!   OLLAMA_HOST                     Ollama address (default http://localhost:11434)

pub mod space;

use serde_json::Value;
use std::env::VarError;
use std::time::Duration;

pub const DEFAULT_LOCAL_MODEL: &str = "pulse-local";
pub const DEFAULT_LOCAL_EMBED_MODEL: &str = "embeddinggemma";
pub const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

/// The value handed out instead of a real API key in local mode. Ollama ignores it.
pub const LOCAL_KEY: &str = "ollama-local";

/// Local generation is slower than the cloud (one model, one GPU, requests queue),
/// so no call gets less than this in local mode.
pub const LOCAL_MIN_TIMEOUT: Duration = Duration::from_secs(600);
pub const LOCAL_INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn is_local() -> bool {
    std::env::var("PULSE_LLM")
        .map(|v| v.trim().eq_ignore_ascii_case("local"))
        .unwrap_or(false)
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn ollama_url() -> String {
    let host = env_or("OLLAMA_HOST", "http://localhost:11434");
    let host = if host.contains("://") { host } else { format!("http://{host}") };
    host.trim_end_matches('/').to_string()
}

pub fn local_model() -> String {
    env_or("PULSE_LOCAL_MODEL", DEFAULT_LOCAL_MODEL)
}

pub fn local_embed_model() -> String {
    env_or("PULSE_LOCAL_EMBED_MODEL", DEFAULT_LOCAL_EMBED_MODEL)
}

/// Drop-in for `std::env::var` on an API-key variable: in local mode every key
/// "exists", so the existing "key missing" checks don't block local runs.
pub fn api_key(var: &str) -> Result<String, VarError> {
    if is_local() {
        return Ok(LOCAL_KEY.to_string());
    }
    std::env::var(var)
}

/// Anthropic Messages endpoint.
pub fn messages_url() -> String {
    if is_local() {
        format!("{}/v1/messages", ollama_url())
    } else {
        ANTHROPIC_MESSAGES_URL.to_string()
    }
}

/// OpenAI-style chat completions endpoint (Groq in the cloud).
pub fn chat_completions_url(cloud: &str) -> String {
    if is_local() {
        format!("{}/v1/chat/completions", ollama_url())
    } else {
        cloud.to_string()
    }
}

/// OpenAI-style embeddings endpoint (Voyage in the cloud).
pub fn embeddings_url(cloud: &str) -> String {
    if is_local() {
        format!("{}/v1/embeddings", ollama_url())
    } else {
        cloud.to_string()
    }
}

/// The model to put in a request: the local one in local mode, else `cloud`.
pub fn model(cloud: &str) -> String {
    if is_local() { local_model() } else { cloud.to_string() }
}

pub fn embed_model(cloud: &str) -> String {
    if is_local() { local_embed_model() } else { cloud.to_string() }
}

/// Request timeout: unchanged in the cloud, at least [`LOCAL_MIN_TIMEOUT`] locally.
pub fn timeout(cloud: Duration) -> Duration {
    if is_local() { cloud.max(LOCAL_MIN_TIMEOUT) } else { cloud }
}

/// Timeout for a call a person is waiting on (search rewrite, rerank). These
/// already degrade gracefully on timeout, so locally they get a bit more room
/// but never the batch budget of [`timeout`].
pub fn interactive_timeout(cloud: Duration) -> Duration {
    if is_local() { cloud.max(LOCAL_INTERACTIVE_TIMEOUT) } else { cloud }
}

/// Anthropic Messages body ready to send. In local mode the model is swapped
/// for the local one and thinking is turned off: these prompts were written for
/// non-thinking Haiku/Sonnet, and a thinking block would both eat `max_tokens`
/// and arrive as `content[0]`, where every caller reads the answer.
pub fn messages_body(body: &Value) -> Value {
    let mut body = body.clone();
    if is_local()
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("model".into(), Value::String(local_model()));
        obj.insert("thinking".into(), serde_json::json!({ "type": "disabled" }));
    }
    body
}

/// Embeddings request body: Voyage's shape in the cloud; locally the
/// OpenAI-style shape Ollama takes, cut to 512 dims to match the stored vectors
/// (EmbeddingGemma is Matryoshka-trained, so the first 512 dims are a valid embedding).
pub fn embeddings_body(cloud_model: &str, texts: &[String], input_type: &str) -> Value {
    if !is_local() {
        return serde_json::json!({ "model": cloud_model, "input": texts, "input_type": input_type });
    }
    let is_query = input_type == "query";
    let input: Vec<String> = texts.iter().map(|t| embed_text(t, is_query)).collect();
    serde_json::json!({ "model": local_embed_model(), "input": input, "dimensions": EMBEDDING_DIMS })
}

pub const EMBEDDING_DIMS: usize = 512;

/// Text to embed. EmbeddingGemma was trained with task prefixes and retrieves
/// noticeably better with them; Voyage takes `input_type` instead, so the cloud
/// text is untouched.
pub fn embed_text(text: &str, is_query: bool) -> String {
    if !is_local() {
        return text.to_string();
    }
    if is_query {
        format!("task: search result | query: {text}")
    } else {
        format!("title: none | text: {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests mutate process-wide env vars; run them one at a time.
    static ENV: Mutex<()> = Mutex::new(());

    fn set(k: &str, v: Option<&str>) {
        // SAFETY: serialized by the ENV mutex; no other thread reads these vars.
        unsafe {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// Restores the variables on drop, so a failed assertion can't leak
    /// `PULSE_LLM=local` into the next test.
    struct Restore(Vec<(String, Option<String>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                set(k, v.as_deref());
            }
        }
    }

    fn with_env<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = Restore(vars.iter().map(|(k, _)| (k.to_string(), std::env::var(k).ok())).collect());
        for (k, v) in vars {
            set(k, *v);
        }
        f()
    }

    #[test]
    fn cloud_mode_changes_nothing() {
        with_env(&[("PULSE_LLM", None), ("PULSE_TEST_KEY", None)], || {
            assert!(!is_local());
            assert_eq!(messages_url(), ANTHROPIC_MESSAGES_URL);
            assert_eq!(model("claude-haiku-4-5-20251001"), "claude-haiku-4-5-20251001");
            assert!(api_key("PULSE_TEST_KEY").is_err());
            let body = serde_json::json!({"model": "claude-sonnet-4-6", "max_tokens": 5});
            assert_eq!(messages_body(&body), body);
            assert_eq!(timeout(Duration::from_secs(10)), Duration::from_secs(10));
            assert_eq!(interactive_timeout(Duration::from_secs(3)), Duration::from_secs(3));
            assert_eq!(embed_text("x", true), "x");
            let b = embeddings_body("voyage-3-lite", &["a".into()], "document");
            assert_eq!(b, serde_json::json!({"model": "voyage-3-lite", "input": ["a"], "input_type": "document"}));
        });
    }

    #[test]
    fn local_mode_routes_everything_to_ollama() {
        with_env(
            &[
                ("PULSE_LLM", Some("Local")),
                ("OLLAMA_HOST", Some("127.0.0.1:9999/")),
                ("PULSE_LOCAL_MODEL", None),
                ("PULSE_TEST_KEY", None),
            ],
            || {
                assert!(is_local());
                assert_eq!(messages_url(), "http://127.0.0.1:9999/v1/messages");
                assert_eq!(
                    chat_completions_url("https://api.groq.com/x"),
                    "http://127.0.0.1:9999/v1/chat/completions"
                );
                assert_eq!(model("claude-sonnet-4-6"), DEFAULT_LOCAL_MODEL);
                assert_eq!(embed_model("voyage-3-lite"), DEFAULT_LOCAL_EMBED_MODEL);
                assert_eq!(api_key("PULSE_TEST_KEY").unwrap(), LOCAL_KEY);
                assert_eq!(timeout(Duration::from_secs(10)), LOCAL_MIN_TIMEOUT);
                assert_eq!(timeout(Duration::from_secs(900)), Duration::from_secs(900));
                assert_eq!(interactive_timeout(Duration::from_secs(3)), LOCAL_INTERACTIVE_TIMEOUT);

                let body = serde_json::json!({"model": "claude-sonnet-4-6", "max_tokens": 5});
                let out = messages_body(&body);
                assert_eq!(out["model"], DEFAULT_LOCAL_MODEL);
                assert_eq!(out["thinking"]["type"], "disabled");
                assert_eq!(out["max_tokens"], 5);

                let b = embeddings_body("voyage-3-lite", &["a".into()], "query");
                assert_eq!(b["model"], DEFAULT_LOCAL_EMBED_MODEL);
                assert_eq!(b["dimensions"], 512);
                assert!(b["input"][0].as_str().unwrap().starts_with("task: search result"));
                assert!(b.get("input_type").is_none());

                assert!(embed_text("rates", true).starts_with("task: search result | query: "));
                assert!(embed_text("story", false).starts_with("title: none | text: "));
            },
        );
    }

    #[test]
    fn local_model_override() {
        with_env(&[("PULSE_LLM", Some("local")), ("PULSE_LOCAL_MODEL", Some(" gemma4:e4b "))], || {
            assert_eq!(model("claude-haiku-4-5-20251001"), "gemma4:e4b");
        });
    }
}
