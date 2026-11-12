# jev-mcp

MCP server over [TypeSafe](https://typesafe.ai)'s Jev model, served over streamable HTTP.

Jev does not generate text. It answers typed questions about content you send it: a
yes/no probability, a pick from options you define, or a score on a rubric, each with
calibrated probabilities. One answer takes ~150 ms and costs a fraction of a cent, so an
agent can use it to triage many items and only read the promising ones itself.

## Tools

### `ask`

One or more questions about one piece of content, in a single request.

| Argument | Type | Meaning |
|---|---|---|
| `state` | string, object or array | The content every question is about |
| `questions` | map of name → question | Names are yours; answers come back under the same names |

Three question types, mixed freely:

| `type` | Fields | Answer |
|---|---|---|
| `noul` | `instructions` (required), `criteria: {true, false}` (optional: what counts as yes/no) | `noul`: probability of yes, 0-1 |
| `choice` | `criteria: {name: description}` (1-255), `instructions` | `choice`, `confidence`, `probabilities` per option |
| `score` | `criteria: [level, ...]` (1-10, lowest first), `instructions` | `score` (expected level, 0-based, may fall between levels), `confidence`, `legend`, `probabilities` per level |

```json
{
  "state": "I was charged twice for my subscription. Please refund the extra charge.",
  "questions": {
    "billing": { "type": "noul", "instructions": "Is this message about billing?" },
    "tone":    { "type": "choice", "criteria": { "angry": "hostile", "calm": "neutral or polite" } },
    "urgency": { "type": "score", "criteria": ["Can wait", "This week", "Today"] }
  }
}
```

The reply is Jev's response unchanged:

```json
{
  "model": "jev-1.14.0",
  "answers": {
    "billing": { "type": "noul", "noul": 0.99 },
    "tone":    { "type": "choice", "choice": "calm", "confidence": 1.0, "probabilities": { "angry": 0.0, "calm": 1.0 } },
    "urgency": { "type": "score", "score": 1.19, "confidence": 0.66,
                 "legend": { "0": "Can wait", "1": "This week", "2": "Today" },
                 "probabilities": { "0": 0.02, "1": 0.77, "2": 0.21 } }
  },
  "usage": { "input_tokens": 370, "output_tokens": 65 }
}
```

### `rank`

The same yes/no question asked of many candidates, returned best first. Use it to decide
which files, functions, log chunks or diff hunks deserve a full read.

| Argument | Type | Meaning |
|---|---|---|
| `question` | string | A yes/no question, e.g. "Is this code involved in refreshing OAuth tokens?" |
| `candidates` | map of id → text | 1-200 entries; the id is yours (a path, a symbol, a line number) |
| `top` | integer, optional | Return only the best `top` |

Each candidate is a separate Jev request, eight in flight at a time, so a call costs one
request per candidate. The reply is sorted by the probability of yes, ties by id:

```json
[
  { "id": "auth/refresh.rs", "yes": 0.98 },
  { "id": "auth/session.rs", "yes": 0.97 },
  { "id": "ui/button.css",   "yes": 0.02 }
]
```

If any candidate fails, the whole call fails.

### Reading the numbers

Values near 0 or 1 are confident, values near 0.5 mean Jev is unsure. A sensible caller
acts on the confident ends and looks at the middle itself. Probabilities are only
comparable within one model version, so pin `JEV_MODEL` once you build thresholds on them.

### Errors

| Cause | Returned to the client |
|---|---|
| Jev rejects the request (400/422), e.g. an empty `criteria` | `invalid_params` carrying Jev's own detail, naming the field |
| Rate limit (429) | `internal_error`: "Jev is rate limiting this key" |
| Bad key, 5xx, network, timeout (30 s) | `internal_error`: "the Jev call failed"; the detail is logged, not returned |

## Configuration

| Variable | Default | |
|---|---|---|
| `TYPESAFE_API_KEY` | required | From https://console.typesafe.ai/settings/keys |
| `JEV_MODEL` | `jev-latest` | Pin a version (e.g. `jev-1.14.0`) once you rely on its probabilities |
| `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` | |
| `JEV_MCP_HOST` / `JEV_MCP_PORT` | `0.0.0.0` / `8000` | |
| `JEV_MCP_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | `Host` headers `/mcp` answers. Set it to the name your MCP client uses (`jev-mcp` behind MCPHub); otherwise every call gets a 403 while `/healthz` stays green |
| `JEV_MCP_LOG_LEVEL` | `INFO` | |

## Deploying

`deploy/compose.yaml` is an example; put the key in `.env` beside it (see `.env.example`).
The image is `FROM scratch` with one static binary; `jev-mcp --health-check` is its
healthcheck. Endpoints: `/mcp` (streamable HTTP, stateless) and `/healthz`.

Releases: bump `version` in `Cargo.toml` and merge to main. `tag.yml` tags it and
`image.yml` publishes `ghcr.io/rgryta/jev-mcp:X.Y.Z` and `:latest`.

## Development

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release --bins
```

The tests run against a fake Jev endpoint and never need a key.
