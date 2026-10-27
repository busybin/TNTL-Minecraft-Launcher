//! Process-wide proxy configuration.
//!
//! Every HTTP client in the workspace is built by [`crate::Http::new`], so one
//! global setting covers installs, Modrinth, runtimes and pack downloads.

use std::sync::{OnceLock, RwLock};

use crate::http::SourceError;

/// How outgoing requests pick a proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyMode {
    /// Follow `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` from the environment.
    #[default]
    System,
    /// Never use a proxy, even when the environment asks for one.
    None,
    /// Explicit HTTP proxy, e.g. `http://127.0.0.1:7890`.
    Http,
    /// Explicit SOCKS5 proxy, e.g. `socks5://127.0.0.1:7890`.
    Socks5,
}

impl ProxyMode {
    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::None => "none",
            Self::Http => "http",
            Self::Socks5 => "socks5",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" | "env" => Some(Self::System),
            "none" | "off" | "direct" => Some(Self::None),
            "http" | "https" => Some(Self::Http),
            "socks5" | "socks" => Some(Self::Socks5),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    pub mode: ProxyMode,
    /// Proxy URL; ignored unless the mode is `http` or `socks5`.
    pub url: Option<String>,
    /// Optional proxy account. Credentials live beside the URL instead of
    /// inside it so error messages can print the URL safely.
    pub username: Option<String>,
    pub password: Option<String>,
}

impl NetworkConfig {
    pub fn from_parts(mode: &str, url: Option<&str>) -> Self {
        Self::from_parts_auth(mode, url, None, None)
    }

    pub fn from_parts_auth(mode: &str, url: Option<&str>, username: Option<&str>, password: Option<&str>) -> Self {
        let mode = ProxyMode::from_key(mode).unwrap_or_default();
        let url = url.map(str::trim).filter(|url| !url.is_empty());
        let username = username.map(str::trim).filter(|name| !name.is_empty());
        Self {
            mode,
            url: url.map(|url| with_scheme(mode, url)),
            // A password without a name cannot be sent by any of the schemes.
            username: username.map(str::to_string),
            password: username.and_then(|_| password.map(str::to_string).filter(|value| !value.is_empty())),
        }
    }
}

/// Accept `127.0.0.1:7890` and other scheme-less input.
fn with_scheme(mode: ProxyMode, url: &str) -> String {
    if url.contains("://") {
        return url.to_string();
    }
    match mode {
        ProxyMode::Socks5 => format!("socks5://{url}"),
        _ => format!("http://{url}"),
    }
}

static CONFIG: OnceLock<RwLock<NetworkConfig>> = OnceLock::new();

fn config() -> &'static RwLock<NetworkConfig> {
    CONFIG.get_or_init(|| RwLock::new(NetworkConfig::default()))
}

pub fn set_network_config(next: NetworkConfig) {
    if let Ok(mut slot) = config().write() {
        *slot = next;
    }
}

pub fn network_config() -> NetworkConfig {
    config().read().map(|slot| slot.clone()).unwrap_or_default()
}

/// Apply the global proxy to a client builder.
pub fn apply_proxy(builder: reqwest::blocking::ClientBuilder) -> Result<reqwest::blocking::ClientBuilder, SourceError> {
    let config = network_config();
    match config.mode {
        ProxyMode::None => Ok(builder.no_proxy()),
        ProxyMode::System => Ok(builder),
        ProxyMode::Http | ProxyMode::Socks5 => {
            let Some(url) = config.url.as_deref() else {
                return Ok(builder);
            };
            let mut proxy = reqwest::Proxy::all(url).map_err(|error| SourceError::Proxy {
                url: url.to_string(),
                message: error.to_string(),
            })?;
            // `basic_auth` writes the account into the proxy URL, which is
            // what both the HTTP tunnel and the SOCKS5 handshake read.
            if let Some(username) = config.username.as_deref() {
                proxy = proxy.basic_auth(username, config.password.as_deref().unwrap_or_default());
            }
            Ok(builder.proxy(proxy))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_modes() {
        assert_eq!(ProxyMode::from_key("SOCKS5"), Some(ProxyMode::Socks5));
        assert_eq!(ProxyMode::from_key("off"), Some(ProxyMode::None));
        assert_eq!(ProxyMode::from_key("bogus"), None);
    }

    #[test]
    fn adds_missing_scheme() {
        let config = NetworkConfig::from_parts("socks5", Some(" 127.0.0.1:1080 "));
        assert_eq!(config.url.as_deref(), Some("socks5://127.0.0.1:1080"));
        let config = NetworkConfig::from_parts("http", Some("http://proxy.example:8080"));
        assert_eq!(config.url.as_deref(), Some("http://proxy.example:8080"));
        let config = NetworkConfig::from_parts("none", Some("  "));
        assert!(config.url.is_none());
    }

    #[test]
    fn keeps_credentials_out_of_the_url() {
        let config = NetworkConfig::from_parts_auth("socks5", Some("127.0.0.1:1080"), Some("  user  "), Some("secret"));
        assert_eq!(config.url.as_deref(), Some("socks5://127.0.0.1:1080"));
        assert_eq!(config.username.as_deref(), Some("user"));
        assert_eq!(config.password.as_deref(), Some("secret"));

        // A password without a user name is never sent.
        let config = NetworkConfig::from_parts_auth("http", Some("127.0.0.1:7890"), Some(" "), Some("secret"));
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }

    #[test]
    fn builds_a_client_with_proxy_credentials() {
        set_network_config(NetworkConfig::from_parts_auth(
            "http",
            Some("127.0.0.1:7890"),
            Some("user"),
            Some("pa:ss@word"),
        ));
        let client = crate::Http::new();
        set_network_config(NetworkConfig::default());
        assert!(client.is_ok(), "{:?}", client.err());
    }
}
