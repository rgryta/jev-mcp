use std::env;
use std::fmt;

/// `Debug` prints a redaction, so no `{settings:?}` can leak the API key.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(**********)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{name} is not a number: {value:?}")]
    NotANumber { name: &'static str, value: String },
    #[error("port must be between 1 and 65535")]
    PortOutOfRange,
}

/// rmcp reads an empty allowlist as "answer any `Host`", so an unset variable falls
/// back to loopback and fails closed rather than open.
pub const DEFAULT_ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

#[derive(Debug, Clone)]
pub struct Settings {
    pub api_key: Secret,
    pub base_url: String,
    pub model: String,
    pub host: String,
    pub port: u16,
    pub log_level: String,
    pub allowed_hosts: Vec<String>,
}

impl Settings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(|name| env::var(name).ok())
    }

    /// `set_var` is process-global and unsafe in edition 2024, so tests read from a
    /// closure instead.
    pub fn from_source(get: impl Fn(&'static str) -> Option<String>) -> Result<Self, ConfigError> {
        let (host, port) = address_from_source(&get)?;
        Ok(Self {
            api_key: Secret(
                get("TYPESAFE_API_KEY")
                    .filter(|key| !key.trim().is_empty())
                    .ok_or(ConfigError::Missing("TYPESAFE_API_KEY"))?,
            ),
            base_url: get("TYPESAFE_BASE_URL")
                .unwrap_or_else(|| "https://api.typesafe.ai".to_owned())
                .trim_end_matches('/')
                .to_owned(),
            model: get("JEV_MODEL").unwrap_or_else(|| "jev-latest".to_owned()),
            host,
            port,
            log_level: get("JEV_MCP_LOG_LEVEL").unwrap_or_else(|| "INFO".to_owned()),
            allowed_hosts: parse_allowed_hosts(&get("JEV_MCP_ALLOWED_HOSTS").unwrap_or_default()),
        })
    }
}

/// Read on its own so `--health-check` needs no API key.
pub fn address_from_env() -> Result<(String, u16), ConfigError> {
    address_from_source(&|name| env::var(name).ok())
}

fn address_from_source(
    get: &impl Fn(&'static str) -> Option<String>,
) -> Result<(String, u16), ConfigError> {
    let port = match get("JEV_MCP_PORT") {
        Some(raw) => {
            let parsed: u32 = raw.trim().parse().map_err(|_| ConfigError::NotANumber {
                name: "JEV_MCP_PORT",
                value: raw.clone(),
            })?;
            u16::try_from(parsed)
                .ok()
                .filter(|port| *port != 0)
                .ok_or(ConfigError::PortOutOfRange)?
        }
        None => 8000,
    };
    Ok((
        get("JEV_MCP_HOST").unwrap_or_else(|| "0.0.0.0".to_owned()),
        port,
    ))
}

pub fn parse_allowed_hosts(raw: &str) -> Vec<String> {
    let configured: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .collect();
    if configured.is_empty() {
        return DEFAULT_ALLOWED_HOSTS.map(str::to_owned).to_vec();
    }
    configured
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(vars: &[(&'static str, &'static str)]) -> Result<Settings, ConfigError> {
        let vars = vars.to_vec();
        Settings::from_source(move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn the_api_key_never_prints() {
        let settings = settings(&[("TYPESAFE_API_KEY", "ts-hunter2")]).unwrap();
        assert!(!format!("{settings:?}").contains("hunter2"));
        assert_eq!(settings.api_key.expose(), "ts-hunter2");
    }

    #[test]
    fn a_missing_or_blank_key_is_an_error() {
        assert!(matches!(settings(&[]), Err(ConfigError::Missing(_))));
        assert!(matches!(
            settings(&[("TYPESAFE_API_KEY", "  ")]),
            Err(ConfigError::Missing(_))
        ));
    }

    #[test]
    fn defaults_and_overrides_read_the_documented_names() {
        let defaults = settings(&[("TYPESAFE_API_KEY", "k")]).unwrap();
        assert_eq!(defaults.base_url, "https://api.typesafe.ai");
        assert_eq!(defaults.model, "jev-latest");
        assert_eq!(defaults.port, 8000);

        let overridden = settings(&[
            ("TYPESAFE_API_KEY", "k"),
            ("TYPESAFE_BASE_URL", "http://fake:1/"),
            ("JEV_MODEL", "jev-1.13.0"),
            ("JEV_MCP_PORT", "9000"),
            ("JEV_MCP_ALLOWED_HOSTS", "jev-mcp"),
        ])
        .unwrap();
        assert_eq!(overridden.base_url, "http://fake:1");
        assert_eq!(overridden.model, "jev-1.13.0");
        assert_eq!(overridden.port, 9000);
        assert_eq!(overridden.allowed_hosts, ["jev-mcp"]);
    }

    #[test]
    fn a_bad_port_is_reported() {
        for raw in ["0", "70000", "soon"] {
            let only = move |name: &'static str| match name {
                "TYPESAFE_API_KEY" => Some("k".to_owned()),
                "JEV_MCP_PORT" => Some(raw.to_owned()),
                _ => None,
            };
            assert!(Settings::from_source(only).is_err(), "{raw}");
        }
    }

    #[test]
    fn a_blank_allowlist_falls_back_to_loopback_not_to_nothing() {
        for raw in ["", "  ", " , , "] {
            assert_eq!(parse_allowed_hosts(raw), DEFAULT_ALLOWED_HOSTS, "{raw:?}");
        }
        assert_eq!(parse_allowed_hosts("a, b:8000"), ["a", "b:8000"]);
    }
}
