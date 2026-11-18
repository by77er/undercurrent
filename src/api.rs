use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

const API_VERSION: &str = "2026-10-21";
const ATTEMPTS: u32 = 4;

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
        let key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|key| !key.is_empty())?;
        let base = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|base| !base.is_empty());
        let base = base.unwrap_or_else(|| "https://api.anthropic.com".to_string());
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(600))
            .try_proxy_from_env(true)
            .build();
        Some(Client {
            agent,
            url: format!("{}/v1/messages", base.trim_end_matches('/')),
            key,
        })
    }

    pub fn messages(&self, body: &Value, beta: Option<&str>) -> Result<Reply> {
        let mut wait = Duration::from_secs(2);
        for attempt in 1..=ATTEMPTS {
            let mut request = self
                .agent
                .post(&self.url)
                .set("x-api-key", &self.key)
                .set("anthropic-version", API_VERSION);
            if let Some(beta) = beta {
                request = request.set("anthropic-beta", beta);
            }
            let failure = match request.send_json(body) {
                Ok(response) => {
                    let value: Value = response
                        .into_json()
                        .context("the Claude API returned a body that is not JSON")?;
                    return Ok(Reply {
                        stop_reason: value["stop_reason"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        content: value["content"].as_array().cloned().unwrap_or_default(),
                    });
                }
                Err(ureq::Error::Status(code, response)) => {
                    let retry_after = response
                        .header("retry-after")
                        .and_then(|v| v.parse::<u64>().ok());
                    let text = response.into_string().unwrap_or_default();
                    if code != 429 && code < 500 {
                        bail!("the Claude API rejected the request with HTTP {code}: {text}");
                    }
                    if let Some(seconds) = retry_after {
                        wait = Duration::from_secs(seconds.min(60));
                    }
                    format!("HTTP {code}: {text}")
                }
                Err(ureq::Error::Transport(transport)) => transport.to_string(),
            };
            if attempt == ATTEMPTS {
                bail!(
                    "the Claude API failed {ATTEMPTS} times, last with {failure}; re-run the check"
                );
            }
            std::thread::sleep(wait);
            wait *= 2;
        }
        bail!("the Claude API was never called; re-run the check")
    }
}
