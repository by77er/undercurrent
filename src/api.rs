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
    send(agent, "POST", url, headers, Some(body))
}

pub fn send(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> Result<Value, Failure> {
    let mut wait = Duration::from_secs(2);
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        let request = headers
            .iter()
            .fold(agent.request(method, url), |request, (name, value)| {
                request.set(name, value)
            });
        let outcome = match body {
            Some(body) => request.send_json(body),
            None => request.call(),
        };
        match outcome {
            Ok(response) => {
                let text = response
                    .into_string()
                    .map_err(|err| Failure::Unavailable(format!("an unreadable body: {err}")))?;
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                return serde_json::from_str(&text).map_err(|err| {
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

#[derive(Debug)]
pub struct OutOfCredit(pub String);

impl std::fmt::Display for OutOfCredit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} reports that the account has no credit left; add credit to have semantic rules judged again", self.0)
    }
}

impl std::error::Error for OutOfCredit {}

const NO_CREDIT_SIGNS: &[&str] = &[
    "billing_error",
    "credit balance",
    "insufficient credit",
    "out of credit",
    "insufficient_quota",
    "insufficient funds",
    "payment required",
];

fn out_of_credit(failure: &Failure) -> bool {
    let (code, text) = match failure {
        Failure::Rejected { code, body } => (Some(*code), body),
        Failure::Unavailable(detail) => (None, detail),
    };
    let lowered = text.to_lowercase();
    code == Some(402) || NO_CREDIT_SIGNS.iter().any(|sign| lowered.contains(sign))
}

pub fn ran_out_of_credit(err: &anyhow::Error) -> Option<&str> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<OutOfCredit>())
        .map(|out| out.0.as_str())
}

pub fn describe(service: &str, failure: Failure) -> anyhow::Error {
    if out_of_credit(&failure) {
        return anyhow::Error::new(OutOfCredit(service.to_string()));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(code: u16, body: &str) -> Failure {
        Failure::Rejected {
            code,
            body: body.to_string(),
        }
    }

    #[test]
    fn exhausted_credit_is_told_apart_from_other_failures() {
        assert!(out_of_credit(&rejected(402, "{}")));
        assert!(out_of_credit(&rejected(
            400,
            r#"{"error":{"message":"Your credit balance is too low to access the API"}}"#
        )));
        assert!(out_of_credit(&Failure::Unavailable(
            "4 failed attempts, last with HTTP 429: insufficient_quota".to_string()
        )));
        assert!(!out_of_credit(&rejected(401, "invalid x-api-key")));
        assert!(!out_of_credit(&rejected(429, "rate limit exceeded")));
        assert!(!out_of_credit(&Failure::Unavailable(
            "connection reset".to_string()
        )));
    }

    #[test]
    fn the_cause_survives_added_context() {
        let err = describe("the Claude API", rejected(402, "{}")).context("src/a.rs");
        assert_eq!(ran_out_of_credit(&err), Some("the Claude API"));
        assert_eq!(
            ran_out_of_credit(&describe("the Claude API", rejected(500, "boom"))),
            None
        );
    }
}
