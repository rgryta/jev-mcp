//! The Jev client against a fake System One endpoint: request shape, auth and error mapping.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use jev_mcp::config::Secret;
use jev_mcp::jev::{Jev, JevError, Question};
use serde_json::{Value, json};

type Seen = Arc<Mutex<Vec<(Option<String>, Value)>>>;

/// Answers `noul` with 0.9 when the state mentions "oauth", else 0.1; a state of
/// "bad" gets a 422, "busy" a 429.
async fn fake(State(seen): State<Seen>, headers: HeaderMap, body: String) -> (StatusCode, String) {
    let request: Value = serde_json::from_str(&body).unwrap();
    let auth = headers
        .get("authorization")
        .map(|value| value.to_str().unwrap().to_owned());
    seen.lock().unwrap().push((auth, request.clone()));
    let state = request["state"].as_str().unwrap_or_default();
    match state {
        "bad" => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                r#"{"detail":"nope"}"#.into(),
            );
        }
        "busy" => return (StatusCode::TOO_MANY_REQUESTS, String::new()),
        _ => {}
    }
    let yes = if state.contains("oauth") { 0.9 } else { 0.1 };
    let answers: serde_json::Map<String, Value> = request["questions"]
        .as_object()
        .unwrap()
        .keys()
        .map(|name| (name.clone(), json!({"type": "noul", "noul": yes})))
        .collect();
    let response = json!({
        "model": request["model"],
        "answers": answers,
        "usage": {"input_tokens": 1, "output_tokens": 1},
    });
    (StatusCode::OK, response.to_string())
}

async fn start() -> (Jev, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route("/v1/systemone", post(fake))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        Jev::new(&base, Secret::new("ts-key"), "jev-1.13.0".to_owned()),
        seen,
    )
}

#[tokio::test]
async fn ask_sends_bearer_model_state_and_tagged_questions() {
    let (jev, seen) = start().await;
    let questions = BTreeMap::from([
        (
            "auth".to_owned(),
            Question::Noul {
                instructions: "About auth?".to_owned(),
                criteria: None,
            },
        ),
        (
            "urgency".to_owned(),
            Question::Score {
                instructions: None,
                criteria: vec!["low".to_owned(), "high".to_owned()],
            },
        ),
    ]);
    let answer = jev
        .ask(&json!("refresh oauth token"), &questions)
        .await
        .unwrap();
    assert_eq!(answer["answers"]["auth"]["noul"], 0.9);

    let (auth, request) = seen.lock().unwrap().pop().unwrap();
    assert_eq!(auth.as_deref(), Some("Bearer ts-key"));
    assert_eq!(
        request,
        json!({
            "model": "jev-1.13.0",
            "state": "refresh oauth token",
            "questions": {
                "auth": {"type": "noul", "instructions": "About auth?"},
                "urgency": {"type": "score", "criteria": ["low", "high"]},
            },
        })
    );
}

#[tokio::test]
async fn yes_returns_the_probability() {
    let (jev, _) = start().await;
    assert_eq!(jev.yes("oauth flow", "q").await.unwrap(), 0.9);
    assert_eq!(jev.yes("css", "q").await.unwrap(), 0.1);
}

#[tokio::test]
async fn validation_failures_carry_the_detail_and_429_is_rate_limited() {
    let (jev, _) = start().await;
    match jev.yes("bad", "q").await {
        Err(JevError::Rejected(detail)) => assert!(detail.contains("nope"), "{detail}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        jev.yes("busy", "q").await,
        Err(JevError::RateLimited)
    ));
}

#[tokio::test]
async fn an_unreachable_endpoint_is_an_upstream_error() {
    let jev = Jev::new("http://127.0.0.1:1", Secret::new("k"), "m".to_owned());
    assert!(matches!(
        jev.yes("x", "q").await,
        Err(JevError::Upstream(_))
    ));
}
