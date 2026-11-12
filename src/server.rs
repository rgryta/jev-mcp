use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use futures::{StreamExt, TryStreamExt, stream};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, ErrorData, Implementation, ServerCapabilities, ServerConfig,
};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Settings;
use crate::jev::{Jev, JevError, Question};

/// Each candidate is its own billed request; this bounds what one call can spend.
pub const MAX_CANDIDATES: usize = 200;
const RANK_CONCURRENCY: usize = 8;

pub const INSTRUCTIONS: &str = "\
Typed, calibrated judgments from TypeSafe's Jev model: fast and cheap enough to filter
or classify many items before reading any of them in full. Answers carry probabilities;
treat values near 0.5 as \"unsure\" and look at those items yourself.
";

/// Tool names are hyphenated: MCPHub joins them to the server name with `-`.
#[derive(Clone)]
pub struct JevMcp {
    jev: Jev,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// The content every question is about: text, or any JSON object/array.
    pub state: Value,
    /// Questions keyed by a name you choose; answers come back under the same names.
    /// Mix `noul` (yes/no), `choice` and `score` freely.
    pub questions: BTreeMap<String, Question>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RankArgs {
    /// A yes/no question asked of every candidate, e.g. "Is this code involved in
    /// refreshing OAuth tokens?".
    pub question: String,
    /// Candidates keyed by an id you choose (a path, a symbol, a log line number),
    /// each mapped to the text Jev should judge.
    pub candidates: BTreeMap<String, String>,
    /// Return only the best `top` candidates.
    pub top: Option<usize>,
}

fn failure(error: JevError) -> ErrorData {
    match error {
        JevError::Rejected(detail) => ErrorData::invalid_params(detail, None),
        JevError::RateLimited => ErrorData::internal_error(error.to_string(), None),
        JevError::Upstream(detail) => {
            tracing::error!(%detail, "Jev call failed");
            ErrorData::internal_error("the Jev call failed".to_owned(), None)
        }
    }
}

fn json_result(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value.to_string())])
}

/// Highest probability first; ties keep id order so the output is stable.
pub fn ranked(mut scored: Vec<(String, f64)>, top: Option<usize>) -> Value {
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(top.unwrap_or(usize::MAX));
    Value::Array(
        scored
            .into_iter()
            .map(|(id, yes)| json!({ "id": id, "yes": yes }))
            .collect(),
    )
}

#[tool_router]
impl JevMcp {
    pub fn new(jev: Jev) -> Self {
        Self { jev }
    }

    #[tool(
        name = "ask",
        description = "Ask Jev one or more typed questions about one piece of content. \
                       `noul` answers with the probability of yes; `choice` with the chosen \
                       key, its confidence and every option's probability; `score` with the \
                       expected level (0-based), confidence and per-level probabilities.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn ask(
        &self,
        Parameters(args): Parameters<AskArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.questions.is_empty() {
            return Err(ErrorData::invalid_params(
                "at least one question is required",
                None,
            ));
        }
        let answer = self
            .jev
            .ask(&args.state, &args.questions)
            .await
            .map_err(failure)?;
        Ok(json_result(&answer))
    }

    #[tool(
        name = "rank",
        description = "Ask the same yes/no question of many candidates in parallel and get \
                       them back sorted by the probability of yes, highest first, as \
                       `[{id, yes}]`. Use it to pick which files, functions, log chunks or \
                       diff hunks deserve a full read.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn rank(
        &self,
        Parameters(args): Parameters<RankArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.candidates.is_empty() || args.candidates.len() > MAX_CANDIDATES {
            return Err(ErrorData::invalid_params(
                format!("between 1 and {MAX_CANDIDATES} candidates are required"),
                None,
            ));
        }
        let jev = &self.jev;
        let question = args.question.as_str();
        let scored: Vec<(String, f64)> =
            stream::iter(args.candidates)
                .map(|(id, content)| async move {
                    jev.yes(&content, question).await.map(|yes| (id, yes))
                })
                .buffer_unordered(RANK_CONCURRENCY)
                .try_collect()
                .await
                .map_err(failure)?;
        Ok(json_result(&ranked(scored, args.top)))
    }
}

#[tool_handler]
impl ServerHandler for JevMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("jev-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS.to_string())
    }
}

/// `/mcp` plus `/healthz`. Only `/mcp` sits behind the `Host` allowlist.
pub fn build_router(jev: Jev, allowed_hosts: Vec<String>) -> Router {
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    config.allowed_hosts = allowed_hosts;
    let service = StreamableHttpService::new(
        move || Ok(JevMcp::new(jev.clone())),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route("/healthz", get(async || "ok"))
        .nest_service("/mcp", service)
}

pub async fn run(settings: &Settings) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let jev = Jev::new(
        &settings.base_url,
        settings.api_key.clone(),
        settings.model.clone(),
    );
    let address: SocketAddr = format!("{}:{}", settings.host, settings.port).parse()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    // A Host mismatch looks like a healthy container answering every call with 403.
    tracing::info!(
        allowed_hosts = %settings.allowed_hosts.join(", "),
        model = %settings.model,
        %address,
        "serving Jev over MCP"
    );
    axum::serve(listener, build_router(jev, settings.allowed_hosts.clone()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Probes our own `/healthz` over raw TCP: the scratch image has no shell or curl.
pub async fn health_check(host: &str, port: u16) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let host = match host {
        "0.0.0.0" | "::" | "" => "127.0.0.1",
        host => host,
    };
    let target = format!("{host}:{port}");
    let mut stream = tokio::net::TcpStream::connect(&target)
        .await
        .map_err(|error| format!("could not connect to {target}: {error}"))?;
    stream
        .write_all(
            format!("GET /healthz HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .map_err(|error| format!("could not send the probe: {error}"))?;
    let mut status = String::new();
    BufReader::new(stream)
        .read_line(&mut status)
        .await
        .map_err(|error| format!("no answer from {target}: {error}"))?;
    if status.contains(" 200 ") {
        return Ok(());
    }
    Err(format!("/healthz answered {}", status.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_registered_hyphen_safe_described_and_read_only() {
        let tools = JevMcp::tool_router().list_all();
        let mut names: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
        names.sort();
        assert_eq!(names, ["ask", "rank"]);
        for tool in tools {
            assert!(!tool.name.contains('_'), "{}", tool.name);
            assert!(tool.description.is_some(), "{}", tool.name);
            let hints = tool.annotations.expect("annotations");
            assert_eq!(hints.read_only_hint, Some(true), "{}", tool.name);
        }
    }

    #[test]
    fn ranking_is_descending_stable_and_truncated() {
        let scored = vec![
            ("b".to_owned(), 0.5),
            ("a".to_owned(), 0.5),
            ("c".to_owned(), 0.9),
            ("d".to_owned(), 0.1),
        ];
        assert_eq!(
            ranked(scored, Some(3)),
            json!([
                {"id": "c", "yes": 0.9},
                {"id": "a", "yes": 0.5},
                {"id": "b", "yes": 0.5},
            ])
        );
    }

    #[tokio::test]
    async fn the_health_check_target_is_never_the_wildcard_address() {
        let error = health_check("0.0.0.0", 1).await.unwrap_err();
        assert!(error.contains("127.0.0.1:1"), "{error}");
    }
}
