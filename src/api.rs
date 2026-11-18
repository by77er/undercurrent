use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

const API_VERSION: &str = "2026-10-21";
const ATTEMPTS: u32 = 4;

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(600))
        .try_proxy_from_env(true)
        .build()
}

pub fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub enum Failure {
    Rejected { code: u16, body: String },
    Unavailable(String),
}

pub fn post(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> Result<Value, Failure> {
    let mut wait = Duration::from_secs(2);
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        let request = headers
            .iter()
            .fold(agent.post(url), |request, (name, value)| {
                request.set(name, value)
            });
        match request.send_json(body) {
            Ok(response) => {
                return response.into_json().map_err(|err| {
                    Failure::Unavailable(format!("a body that is not JSON: {err}"))
                });
            }
            Err(ureq::Error::Status(code, response)) => {
                let retry_after = response
                    .header("retry-after")
                    .and_then(|v| v.parse::<u64>().ok());
                let body = response.into_string().unwrap_or_default();
                if code != 429 && code < 500 {
                    return Err(Failure::Rejected { code, body });
                }
                if let Some(seconds) = retry_after {
                    wait = Duration::from_secs(seconds.min(60));
                }
                last = format!("HTTP {code}: {body}");
            }
            Err(ureq::Error::Transport(transport)) => last = transport.to_string(),
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(wait);
            wait *= 2;
        }
    }
    Err(Failure::Unavailable(format!(
        "{ATTEMPTS} failed attempts, last with {last}"
    )))
}

pub fn describe(service: &str, failure: Failure) -> anyhow::Error {
    match failure {
        Failure::Rejected { code, body } => {
            anyhow::anyhow!("{service} rejected the request with HTTP {code}: {body}; check the API key and request")
        }
        Failure::Unavailable(detail) => {
            anyhow::anyhow!("{service} could not be reached ({detail}); re-run the check")
        }
    }
}

pub struct Client {
    agent: ureq::Agent,
    url: String,
    key: String,
}

pub struct Reply {
    pub stop_reason: String,
    pub content: Vec<Value>,
}

impl Reply {
    pub fn text(&self) -> String {
        let texts = self.content.iter().filter(|block| block["type"] == "text");
        texts.filter_map(|block| block["text"].as_str()).collect()
    }
}

impl Client {
    pub fn from_env() -> Option<Client> {
        let key = env_key("ANTHROPIC_API_KEY")?;
        let base = env_key("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());
        Some(Client {
            agent: agent(),
            url: format!("{}/v1/messages", base.trim_end_matches('/')),
            key,
        })
    }

    pub fn messages(&self, body: &Value, beta: Option<&str>) -> Result<Reply> {
        let mut headers = vec![
            ("x-api-key", self.key.as_str()),
            ("anthropic-version", API_VERSION),
        ];
        headers.extend(beta.map(|beta| ("anthropic-beta", beta)));
        let value = post(&self.agent, &self.url, &headers, body)
            .map_err(|failure| describe("the Claude API", failure))?;
        if value["content"].is_null() {
            bail!("the Claude API returned no content; re-run the check");
        }
        Ok(Reply {
            stop_reason: value["stop_reason"]
                .as_str()
                .context("the Claude API reply has no stop reason; re-run the check")?
                .to_string(),
            content: value["content"].as_array().cloned().unwrap_or_default(),
        })
    }
}
