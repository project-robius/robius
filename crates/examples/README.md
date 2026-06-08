# Robius Examples

This directory contains runnable example apps for Robius crates.

It is intentionally a standalone Cargo package rather than a member of the
top-level Robius workspace, so run commands from this directory.

## Share Example

Desktop:

```sh
cargo run
```

iOS simulator:

```sh
cargo makepad apple ios --org=rs.robius --app=ShareExample run-sim -p robius-examples --release
```

iOS device:

```sh
cargo makepad apple list
cargo makepad apple ios \
  --profile=<profile-prefix> \
  --cert=<cert-prefix> \
  --device=<device-prefix> \
  --org=rs.robius \
  --app=ShareExample \
  run-device -p robius-examples --release
```

Android:

```sh
cargo makepad android --abi=aarch64 run -p robius-examples --release
```

Android with a specific attached device:

```sh
cargo makepad android --devices=<adb-device-serial> --abi=aarch64 run -p robius-examples --release
```
