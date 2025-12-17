//! Cross-platform access to system proxy settings.
//!
//! The API is intentionally small and capability-oriented: you describe the
//! desired [`ProxyState`] and the crate applies it using the native facilities
//! of the underlying platform. Only macOS is implemented right now; other
//! platforms fall back to [`Error::Unsupported`].
//!
//! The abstractions are designed to be forward-compatible with additional
//! platforms such as Linux, Windows, Android, iOS, and OpenHarmony.

mod error;
mod sys;

pub use crate::error::{Error, Result};

/// Uniform representation of a proxy endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyEndpoint {
    /// Hostname or IP of the proxy.
    pub host: String,
    /// TCP port on which the proxy listens.
    pub port: u16,
    /// Optional credentials when the proxy requires authentication.
    pub credentials: Option<Credentials>,
}

impl ProxyEndpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            credentials: None,
        }
    }

    pub fn with_credentials(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        password: Option<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            credentials: Some(Credentials {
                username: username.into(),
                password,
            }),
        }
    }
}

/// Credentials for proxies that require authentication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: Option<String>,
}

/// List of hosts, domains, or CIDRs that should bypass the proxy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BypassList {
    pub entries: Vec<String>,
}

impl BypassList {
    pub fn new(entries: impl Into<Vec<String>>) -> Self {
        Self {
            entries: entries.into(),
        }
    }
}

/// Proxy configuration for manual modes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxySettings {
    pub http: Option<ProxyEndpoint>,
    pub https: Option<ProxyEndpoint>,
    pub socks: Option<ProxyEndpoint>,
    pub bypass: BypassList,
}

/// Modes supported by the abstraction layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyMode {
    /// No proxy usage.
    Direct,
    /// User-specified endpoints.
    Manual(ProxySettings),
    /// Proxy auto-configuration (PAC) script URL.
    AutoConfigUrl {
        url: String,
        bypass: BypassList,
    },
    /// WPAD / automatic discovery.
    AutoDiscovery {
        bypass: BypassList,
    },
}

/// High-level proxy state exposed to callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyState {
    pub mode: ProxyMode,
}

impl ProxyState {
    pub fn direct() -> Self {
        Self {
            mode: ProxyMode::Direct,
        }
    }

    pub fn manual(settings: ProxySettings) -> Self {
        Self {
            mode: ProxyMode::Manual(settings),
        }
    }

    pub fn pac(url: impl Into<String>, bypass: BypassList) -> Self {
        Self {
            mode: ProxyMode::AutoConfigUrl {
                url: url.into(),
                bypass,
            },
        }
    }

    pub fn auto_discovery(bypass: BypassList) -> Self {
        Self {
            mode: ProxyMode::AutoDiscovery { bypass },
        }
    }
}

/// Entry point for interacting with system proxy settings.
pub struct ProxyManager {
    inner: sys::Manager,
}

impl ProxyManager {
    /// Constructs a manager bound to the current platform.
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: sys::Manager::new()?,
        })
    }

    /// Reads the current proxy state from the system.
    pub fn current(&self) -> Result<ProxyState> {
        self.inner.current()
    }

    /// Applies the given proxy state to the system.
    pub fn apply(&self, state: ProxyState) -> Result<()> {
        self.inner.apply(state)
    }

    /// Convenience helper to disable all proxies.
    pub fn disable(&self) -> Result<()> {
        self.apply(ProxyState::direct())
    }
}
