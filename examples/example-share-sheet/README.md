# robius-share Example

A runnable Makepad example app that demonstrates `robius-share`'s native system
share sheet on desktop, iOS, and Android.

It is intentionally a standalone Cargo package rather than a typical Cargo
example (`cargo run --example ...`), since that structure doesn't work for
Makepad apps built with cargo-makebuild. Run commands from this directory.

## Running

Desktop:

```sh
cargo run
```

iOS simulator:

```sh
cargo makepad apple ios --org=rs.robius --app=ShareExample run-sim -p example-share-sheet --release
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
  run-device -p example-share-sheet --release
```

Android:

```sh
cargo makepad android --abi=aarch64 run -p example-share-sheet --release
```

Android with a specific attached device:

```sh
cargo makepad android --devices=<adb-device-serial> --abi=aarch64 run -p example-share-sheet --release
```
