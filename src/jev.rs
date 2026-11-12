use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use reqwest::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Secret;

// `box` is left out: it needs images and an opt-in header, and no MCP client sends images here.
/// A typed question: `noul` (yes/no), `choice` (pick a key) or `score` (rate on ordered levels).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Yes/no. Answered with `noul`, the probability of yes.
    Noul {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Pick one of `criteria`'s keys; each value describes when that choice applies.
    Choice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        criteria: BTreeMap<String, String>,
    },
    /// Rate against 1-10 ordered levels; position 0 is the lowest score.
    Score {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        criteria: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NoulCriteria {
    /// What counts as yes.
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub yes: Option<String>,
    /// What counts as no.
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub no: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum JevError {
    /// 400/422: the request was wrong, and the detail says how. Safe to show the caller.
    #[error("Jev rejected the request: {0}")]
    Rejected(String),
    #[error("Jev is rate limiting this key")]
    RateLimited,
    /// Auth, 5xx, network or an unreadable body. Logged, not shown.
    #[error("{0}")]
    Upstream(String),
}

#[derive(Clone)]
pub struct Jev {
    http: reqwest::Client,
    endpoint: String,
    api_key: Secret,
    model: String,
}

#[derive(Deserialize)]
struct NoulAnswers {
    answers: HashMap<String, NoulAnswer>,
}

#[derive(Deserialize)]
struct NoulAnswer {
    noul: f64,
}

impl Jev {
    pub fn new(base_url: &str, api_key: Secret, model: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("a client with only a timeout set always builds");
        Self {
            http,
            endpoint: format!("{base_url}/v1/systemone"),
            api_key,
            model,
        }
    }

    /// One System One request; the response body is returned as Jev sent it.
    pub async fn ask(
        &self,
        state: &Value,
        questions: &BTreeMap<String, Question>,
    ) -> Result<Value, JevError> {
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(self.api_key.expose())
            .json(&json!({ "model": self.model, "state": state, "questions": questions }))
            .send()
            .await
            .map_err(|error| JevError::Upstream(format!("request failed: {error}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| JevError::Upstream(format!("unreadable body: {error}")))?;
        match status {
            StatusCode::OK => serde_json::from_str(&body)
                .map_err(|error| JevError::Upstream(format!("non-JSON 200: {error}"))),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                Err(JevError::Rejected(body))
            }
            StatusCode::TOO_MANY_REQUESTS => Err(JevError::RateLimited),
            other => Err(JevError::Upstream(format!("HTTP {other}: {body}"))),
        }
    }

    /// The probability of yes for one question about one piece of content.
    pub async fn yes(&self, content: &str, question: &str) -> Result<f64, JevError> {
        let questions = BTreeMap::from([(
            "q".to_owned(),
            Question::Noul {
                instructions: question.to_owned(),
                criteria: None,
            },
        )]);
        let body = self
            .ask(&Value::String(content.to_owned()), &questions)
            .await?;
        let mut parsed: NoulAnswers = serde_json::from_value(body)
            .map_err(|error| JevError::Upstream(format!("unexpected answer shape: {error}")))?;
        parsed
            .answers
            .remove("q")
            .map(|answer| answer.noul)
            .ok_or_else(|| JevError::Upstream("answer `q` missing".to_owned()))
    }
}
