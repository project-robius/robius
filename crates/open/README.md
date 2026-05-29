# `robius-open`

[![Latest Version](https://img.shields.io/crates/v/robius-open.svg)](https://crates.io/crates/robius-open)
[![Docs](https://docs.rs/robius-open/badge.svg)](https://docs.rs/robius-open/latest/robius_open/)
[![Project Robius Matrix Chat](https://img.shields.io/matrix/robius-general%3Amatrix.org?server_fqdn=matrix.org&style=flat&logo=matrix&label=Project%20Robius%20Matrix%20Chat&color=B7410E)](https://matrix.to/#/#robius:matrix.org)

This crate provides easy Rust interfaces to open URIs across multiple platforms, including:
- macOS (via `NSWorkspace`)
- Android (via `android/content/Intent`)
- Linux (via `xdg-open`)
- Windows (via `start`)
- iOS (via `UIApplication`)

URIs take many different forms: URLs (`http://`), `tel:`, `mailto:`, `file://`, and more (see the [official list of schemes](https://www.iana.org/assignments/uri-schemes/uri-schemes.xhtml)).

## Examples

```rust
use robius_open::Uri;
Uri::new("tel:+61 123 456 789")
   .open()
   .expect("failed to open telephone URI");
```

```rust
use robius_open::Uri;
Uri::new("http://www.google.com")
   .open_with_completion(|success| {
      log!("Opened URI? {success}");
   })
   .expect("failed to open URL");
```
