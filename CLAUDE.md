# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Robius** is a Rust workspace providing cross-platform abstractions for OS APIs and services. It enables app developers to access platform-specific functionality (authentication, location, directories, URI handling) through unified Rust interfaces.


## Build Commands

```bash
cargo build                           # Build all crates
cargo build -p robius-authentication  # Build specific crate
cargo test --workspace                # Run all tests
cargo test -p robius-directories      # Test specific crate
cargo test test_name                  # Run specific test
cargo test -- --nocapture             # Tests with output
cargo doc --no-deps --open            # Generate and view docs
```

Cross-compilation targets:
```bash
cargo build --target aarch64-linux-android
cargo build --target x86_64-apple-darwin
cargo build --target x86_64-pc-windows-gnu
```

## Workspace Structure

```
robius/
├── Cargo.toml              # Workspace root (resolver = "3")
└── crates/
    ├── authentication/     # Biometric/password auth (TouchID, FaceID, Windows Hello, polkit)
    ├── directories/        # Platform directory paths (XDG, Known Folders, etc.)
    ├── location/           # GPS/location data access
    └── open/               # URI opening functionality
```

## Architecture Pattern

All crates follow the same structure:

```
crate/
├── src/
│   ├── lib.rs              # Public API, re-exports from sys
│   ├── error.rs            # Error types
│   └── sys/                # Platform-specific implementations
│       ├── android/        # JNI bindings
│       ├── apple.rs        # iOS/macOS via objc2
│       ├── windows/        # Windows API via windows crate
│       ├── linux.rs        # Linux (polkit, gio)
│       └── unsupported.rs  # Fallback
├── build.rs                # Build script (compiles Java for Android)
└── Cargo.toml
```

**Platform dispatch pattern** using `cfg_if!`:
```rust
cfg_if::cfg_if! {
    if #[cfg(target_os = "android")] {
        mod android;
        pub(crate) use android::*;
    } else if #[cfg(target_vendor = "apple")] {
        mod apple;
        pub(crate) use apple::*;
    }
    // ... more platforms
}
```

## API Patterns

1. **Callback Pattern** (authentication, open):
   ```rust
   context.authenticate(text, &policy, |result| { /* handle */ })
   ```

2. **Handler Trait Pattern** (location):
   ```rust
   impl Handler for MyHandler {
       fn handle(&self, location: Location<'_>);
       fn error(&self, error: Error);
   }
   ```

3. **Builder Pattern** (authentication):
   ```rust
   PolicyBuilder::new().biometrics(Some(BiometricStrength::Strong)).password(true).build()
   ```

## Key Dependencies by Platform

- **Android:** `jni`, `robius-android-env`, `android-build` (build script)
- **Apple (iOS/macOS):** `objc2`, `objc2-*` framework bindings, `block2`, `dispatch2`
- **Windows:** `windows`, `windows-core`
- **Linux:** `polkit`, `gio`

## Platform-Specific Notes

- **Android:** Build scripts compile Java files; requires `android.jar` and `d8.jar`
- **Apple:** Uses `objc2` bindings for native frameworks
- **Linux:** Uses `polkit` for authentication, `xdg-open` for URI handling
- Crates document required manifest entries (Info.plist, AndroidManifest.xml) in their READMEs
