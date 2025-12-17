# `robius-proxy`

This crate provides easy Rust interfaces to get and apply proxy across multiple platforms, including:

- Modes: direct, manual (HTTP/HTTPS/SOCKS), PAC URL, WPAD.
- Platforms:
  1. **macOS** : via `networksetup`/`scutil`; others return
  2. Others(Planned): `Error::Unsupported` until added.

## Usage

```rust
use robius_proxy::{BypassList, ProxyEndpoint, ProxyManager, ProxyMode, ProxySettings, ProxyState};

let manager = ProxyManager::new()?;
let current = manager.current()?;

println!("Current mode: {:?}", current.mode);

let state = ProxyState::manual(ProxySettings {
    http: Some(ProxyEndpoint::new("proxy.local", 8080)),
    https: None,
    socks: None,
    bypass: BypassList::new(vec!["localhost".into(), "127.0.0.1".into()]),
});

manager.apply(state)?;
```

## Examples

- Show current configuration: `cargo run -p robius-proxy --example show_current`
- Set manual proxies (macOS): `cargo run -p robius-proxy --example set_manual`

Examples invoke `networksetup` under the hood, so they may prompt for system
permissions depending on your macOS configuration.

## Platform support

- macOS: implemented via `networksetup` and `scutil`.
- Linux (planned): reserve hooks for NetworkManager, GNOME proxy settings, and
  system-wide environment exports.
- Windows (planned): reserve hooks for WinHTTP/WinINET and per-connection
  proxy configuration.
- Android (planned): reserve hooks for global HTTP proxy settings available to
  device owner apps.
- iOS (planned): reserve hooks for CFNetwork proxy dictionaries.
- OpenHarmony (planned): reserve hooks for system proxy APIs when exposed.

Contributions adding new platforms or improving parsing/validation are welcome.
