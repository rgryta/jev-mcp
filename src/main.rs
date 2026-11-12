use std::process::ExitCode;

use jev_mcp::config::{self, Settings};
use jev_mcp::server;

const USAGE: &str = "\
usage: jev-mcp [--health-check]

  (no arguments)   serve the Jev tools over streamable HTTP at /mcp
  --health-check   probe this container's own /healthz and exit 0 when it answers

Configuration: TYPESAFE_API_KEY (required), TYPESAFE_BASE_URL, JEV_MODEL,
JEV_MCP_HOST, JEV_MCP_PORT, JEV_MCP_ALLOWED_HOSTS, JEV_MCP_LOG_LEVEL.
";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [flag] if flag == "--health-check" => return run_health_check().await,
        [flag] if flag == "--help" || flag == "-h" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    }

    let settings = match Settings::from_env() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| settings.log_level.to_lowercase().into()),
        )
        .init();

    match server::run(&settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "the server stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run_health_check() -> ExitCode {
    let (host, port) = match config::address_from_env() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    match server::health_check(&host, port).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
