//! A client for a language model that speaks the OpenAI chat completions API:
//! a hosted service, or a local one such as Ollama, llama.cpp or vLLM, which
//! all accept the same request. indice only needs an HTTP client for it, so
//! which model, and whether archived text leaves the building, are the
//! operator's choice in `config.yaml` (`llm:`), not something compiled in.
//!
//! Generic on purpose: drafting a finding aid (`describe`) is the first use,
//! and later ones share this client and its settings.

use std::io::Read;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// How long to wait for an answer when `llm.timeout_secs` isn't set. A local
/// model drafting from a full sample can take a minute or more.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// How long to wait for a connection, separately from the answer, so an
/// unreachable service fails in seconds rather than after the whole timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of a response indice will read, so a misbehaving service can't
/// fill memory. A drafted description is a few kilobytes.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Which model, where, and how long to wait: `llm:` in `config.yaml`, checked
/// by [`crate::config::Config::llm_settings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmSettings {
    /// The API's base URL, e.g. `http://localhost:11434/v1`.
    pub endpoint: String,
    /// The model name the service knows.
    pub model: String,
    /// How long to wait for an answer.
    pub timeout: Duration,
}

impl LlmSettings {
    /// The service's host and port, e.g. `localhost:11434`: what indice tells a
    /// curator, and records as provenance, about where text was sent. Never
    /// the path or anything after it, and never the key.
    pub fn service(&self) -> String {
        let rest = self
            .endpoint
            .split_once("://")
            .map_or(self.endpoint.as_str(), |(_, rest)| rest);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        // Credentials embedded in a URL (`user:pass@host`) stay out of it too.
        host.rsplit('@').next().unwrap_or(host).to_string()
    }

    /// Whether requests would cross a network unencrypted: a plain `http://`
    /// endpoint whose host isn't this machine or a private network, so the
    /// key (sent in a header) and the archived text travel in the clear. A
    /// local model on `http://localhost` is the normal case and doesn't count.
    pub fn sends_in_clear(&self) -> bool {
        if !self.endpoint.to_ascii_lowercase().starts_with("http://") {
            return false;
        }
        let service = self.service();
        // Strip the port, keeping a bracketed IPv6 address whole.
        let host = match service.strip_prefix('[') {
            Some(rest) => rest.split(']').next().unwrap_or(rest).to_string(),
            None => service
                .rsplit_once(':')
                .map_or(service.as_str(), |(h, _)| h)
                .to_string(),
        }
        .to_ascii_lowercase();
        if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
            return false;
        }
        // A bare name like `ollama` is a machine on the local network (or a
        // container next door), not a public service.
        if !host.contains('.') && !host.contains(':') {
            return false;
        }
        match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => {
                !(ip.is_loopback() || ip.is_private() || ip.is_link_local())
            }
            Ok(std::net::IpAddr::V6(ip)) => {
                let unique_local = (ip.segments()[0] & 0xfe00) == 0xfc00;
                !(ip.is_loopback() || unique_local)
            }
            Err(_) => true,
        }
    }

    fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.endpoint.trim_end_matches('/'))
    }
}

/// Sends a JSON body and returns the status and the body. A trait so tests
/// can stand in for the service, as the Browsertrix client's `Transport` does.
pub trait ChatTransport: Send + Sync {
    fn post_json(&self, url: &str, bearer: Option<&str>, body: &Value) -> Result<(u16, Vec<u8>)>;
}

/// The real transport: a ureq agent with a connect timeout and an overall one.
pub struct UreqChat {
    agent: ureq::Agent,
}

impl UreqChat {
    pub fn new(timeout: Duration) -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_global(Some(timeout))
                .build()
                .new_agent(),
        }
    }
}

impl ChatTransport for UreqChat {
    fn post_json(&self, url: &str, bearer: Option<&str>, body: &Value) -> Result<(u16, Vec<u8>)> {
        let mut req = self
            .agent
            .post(url)
            .header("Content-Type", "application/json");
        if let Some(key) = bearer {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let resp = req
            .send(body.to_string().as_bytes())
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status().as_u16();
        let mut out = Vec::new();
        resp.into_body()
            .into_reader()
            .take(MAX_RESPONSE_BYTES)
            .read_to_end(&mut out)
            .with_context(|| format!("reading the answer from {url}"))?;
        Ok((status, out))
    }
}

/// A configured model, ready to ask.
pub struct Llm {
    settings: LlmSettings,
    key: Option<String>,
    transport: Box<dyn ChatTransport>,
}

impl Llm {
    /// A client for `settings`, sending `key` as a bearer token when given.
    pub fn new(settings: LlmSettings, key: Option<String>) -> Self {
        let transport = Box::new(UreqChat::new(settings.timeout));
        Self::with_transport(settings, key, transport)
    }

    /// The same, over another transport (a fake, in tests).
    pub fn with_transport(
        settings: LlmSettings,
        key: Option<String>,
        transport: Box<dyn ChatTransport>,
    ) -> Self {
        let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
        Self {
            settings,
            key,
            transport,
        }
    }

    pub fn settings(&self) -> &LlmSettings {
        &self.settings
    }

    /// Ask for a JSON object: `system` says what to do, `user` carries the
    /// material. Returns the object the model answered with.
    ///
    /// Asks for JSON mode (`response_format: json_object`), which the common
    /// servers support. One that refuses the parameter is asked once more
    /// without it, since the prompt asks for JSON too; either way the answer
    /// is parsed leniently (see [`extract_json`]).
    pub fn chat_json(&self, system: &str, user: &str) -> Result<Value> {
        let mut body = json!({
            "model": self.settings.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "temperature": 0.2,
            "response_format": {"type": "json_object"},
        });
        let url = self.settings.completions_url();
        let (mut status, mut bytes) = self.post(&url, &body)?;
        if status == 400 && String::from_utf8_lossy(&bytes).contains("response_format") {
            body.as_object_mut().unwrap().remove("response_format");
            (status, bytes) = self.post(&url, &body)?;
        }
        if !(200..300).contains(&status) {
            bail!(
                "{} answered HTTP {status}: {}",
                self.settings.service(),
                snippet(&bytes)
            );
        }
        let resp: Value = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "{} didn't answer with JSON: {}",
                self.settings.service(),
                snippet(&bytes)
            )
        })?;
        let content = resp
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .with_context(|| {
                format!(
                    "{}'s answer has no message content: {}",
                    self.settings.service(),
                    snippet(&bytes)
                )
            })?;
        extract_json(content)
    }

    fn post(&self, url: &str, body: &Value) -> Result<(u16, Vec<u8>)> {
        self.transport
            .post_json(url, self.key.as_deref(), body)
            .with_context(|| {
                format!(
                    "asking {} ({})",
                    self.settings.service(),
                    self.settings.model
                )
            })
    }
}

/// The JSON object in a model's answer. Models asked for JSON still sometimes
/// wrap it in a Markdown code fence or a sentence of preamble, so this tries
/// the text as it is, then inside a fence, then from the first `{` to the
/// last `}`.
pub fn extract_json(text: &str) -> Result<Value> {
    let text = text.trim();
    let candidates = [
        Some(text),
        text.strip_prefix("```json")
            .or_else(|| text.strip_prefix("```"))
            .and_then(|t| t.trim_end().strip_suffix("```")),
        text.find('{')
            .zip(text.rfind('}'))
            .filter(|(a, b)| a < b)
            .map(|(a, b)| &text[a..=b]),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(candidate.trim()) {
            return Ok(v);
        }
    }
    bail!(
        "the model's answer wasn't a JSON object: {}",
        snippet(text.as_bytes())
    )
}

/// The start of a body, for an error message.
fn snippet(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    match s.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn settings() -> LlmSettings {
        LlmSettings {
            endpoint: "http://localhost:11434/v1/".into(),
            model: "test-model".into(),
            timeout: Duration::from_secs(5),
        }
    }

    /// One request the fake service received.
    #[derive(Clone)]
    struct Sent {
        url: String,
        bearer: Option<String>,
        body: Value,
    }

    /// A fake service: records what it was sent, answers from a script.
    #[derive(Clone, Default)]
    struct Fake {
        sent: Arc<Mutex<Vec<Sent>>>,
        answers: Arc<Mutex<Vec<(u16, String)>>>,
    }
    impl Fake {
        fn answering(answers: &[(u16, &str)]) -> Self {
            let f = Fake::default();
            *f.answers.lock().unwrap() = answers
                .iter()
                .rev()
                .map(|(s, b)| (*s, b.to_string()))
                .collect();
            f
        }
    }
    impl ChatTransport for Fake {
        fn post_json(
            &self,
            url: &str,
            bearer: Option<&str>,
            body: &Value,
        ) -> Result<(u16, Vec<u8>)> {
            self.sent.lock().unwrap().push(Sent {
                url: url.into(),
                bearer: bearer.map(String::from),
                body: body.clone(),
            });
            let (s, b) = self.answers.lock().unwrap().pop().expect("an answer");
            Ok((s, b.into_bytes()))
        }
    }

    fn completion(content: &str) -> String {
        json!({"choices": [{"message": {"role": "assistant", "content": content}}]}).to_string()
    }

    #[test]
    fn the_service_is_host_and_port_only() {
        let mut s = settings();
        assert_eq!(s.service(), "localhost:11434");
        s.endpoint = "https://user:secret@api.example.org/v1?x=1".into();
        assert_eq!(
            s.service(),
            "api.example.org",
            "no path, query or credentials"
        );
    }

    #[test]
    fn plain_http_to_a_public_host_is_flagged_and_local_models_are_not() {
        let at = |endpoint: &str| LlmSettings {
            endpoint: endpoint.into(),
            ..settings()
        };
        for clear in [
            "http://gateway.inference.coop/v1",
            "http://203.0.113.7:8000/v1",
            "HTTP://api.example.org/v1",
        ] {
            assert!(at(clear).sends_in_clear(), "{clear}");
        }
        for fine in [
            "https://gateway.inference.coop/v1",
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:11434/v1",
            "http://192.168.1.20:11434/v1",
            "http://10.0.0.5/v1",
            "http://ollama:11434/v1",
            "http://gpu-box.local/v1",
        ] {
            assert!(!at(fine).sends_in_clear(), "{fine}");
        }
    }

    #[test]
    fn it_posts_a_chat_completion_in_json_mode() {
        let fake = Fake::answering(&[(200, &completion(r#"{"a": 1}"#))]);
        let llm = Llm::with_transport(settings(), Some("k3y".into()), Box::new(fake.clone()));
        assert_eq!(llm.chat_json("sys", "usr").unwrap(), json!({"a": 1}));
        let sent = fake.sent.lock().unwrap();
        let Sent { url, bearer, body } = &sent[0];
        assert_eq!(url, "http://localhost:11434/v1/chat/completions");
        assert_eq!(bearer.as_deref(), Some("k3y"));
        assert_eq!(body["model"], "test-model");
        assert_eq!(
            body["messages"][0],
            json!({"role": "system", "content": "sys"})
        );
        assert_eq!(
            body["messages"][1],
            json!({"role": "user", "content": "usr"})
        );
        assert_eq!(body["response_format"], json!({"type": "json_object"}));
    }

    #[test]
    fn a_blank_key_sends_no_bearer() {
        let fake = Fake::answering(&[(200, &completion("{}"))]);
        let llm = Llm::with_transport(settings(), Some("  ".into()), Box::new(fake.clone()));
        llm.chat_json("s", "u").unwrap();
        assert_eq!(fake.sent.lock().unwrap()[0].bearer, None);
    }

    #[test]
    fn a_service_refusing_json_mode_is_asked_again_without_it() {
        let fake = Fake::answering(&[
            (400, r#"{"error": "unknown field: response_format"}"#),
            (200, &completion(r#"{"ok": true}"#)),
        ]);
        let llm = Llm::with_transport(settings(), None, Box::new(fake.clone()));
        assert_eq!(llm.chat_json("s", "u").unwrap(), json!({"ok": true}));
        let sent = fake.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].body.get("response_format").is_none());
    }

    #[test]
    fn an_error_names_the_service_and_status() {
        let fake = Fake::answering(&[(503, "model is loading")]);
        let llm = Llm::with_transport(settings(), None, Box::new(fake));
        let err = format!("{:#}", llm.chat_json("s", "u").unwrap_err());
        assert!(
            err.contains("localhost:11434") && err.contains("503"),
            "{err}"
        );
        assert!(err.contains("model is loading"), "{err}");
    }

    #[test]
    fn an_answer_without_content_is_an_error() {
        let fake = Fake::answering(&[(200, r#"{"choices": []}"#)]);
        let llm = Llm::with_transport(settings(), None, Box::new(fake));
        assert!(llm.chat_json("s", "u").is_err());
    }

    #[test]
    fn json_is_found_inside_fences_and_preamble() {
        assert_eq!(extract_json(r#"{"a":1}"#).unwrap(), json!({"a": 1}));
        assert_eq!(
            extract_json("```json\n{\"a\":1}\n```").unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            extract_json("```\n{\"a\":1}\n```").unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            extract_json("Here is the draft:\n{\"a\": 1}\nHope that helps.").unwrap(),
            json!({"a": 1})
        );
        assert!(extract_json("no json here").is_err());
        assert!(extract_json("[1, 2]").is_err(), "an array isn't an object");
    }
}
