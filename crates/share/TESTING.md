# Testing `robius-share`

This crate has two kinds of behavior:

- Builder and payload validation, which can be tested automatically.
- Native presentation behavior, which must be tested on real OS shells.

The practical test plan is therefore a small automated gate plus a few native
test cases per platform.

## Automated Gate

Run these from the workspace root:

```sh
cargo test -p robius-share
cargo check -p robius-share --examples
cargo check -p robius-share --target x86_64-unknown-linux-gnu
cargo check -p robius-share --target x86_64-pc-windows-msvc
cargo check -p robius-share --target aarch64-linux-android
cargo check -p robius-share --target aarch64-apple-ios
git diff --check
```

This catches the portable builder contract, example compilation, and the native
FFI surfaces for every supported target.

## Makepad Test App

The native share UI should be tested from a real app window/activity rather
than a plain CLI. This repository includes a local Makepad example app for that:

```sh
cd crates/examples
cargo run
```

Use the buttons in the app to exercise:

- Text payload.
- URL payload.
- Generated local file payload.
- Mixed text + URL + generated local file payload.
- Picked file payload.

The picked-file path is the important Android attachment case: Android file
picker results are `content://` URIs, and the example app passes that URI directly
to `robius-share`.

## CLI Desktop Test Cases

On macOS, Windows, and Linux, run:

```sh
cargo run -p robius-share --example share_example -- text
cargo run -p robius-share --example share_example -- url
cargo run -p robius-share --example share_example -- file
cargo run -p robius-share --example share_example -- mixed
```

Pass a file path after `file` or `mixed` to test a specific attachment:

```sh
cargo run -p robius-share --example share_example -- file /path/to/file.pdf
```

Expected result: the system-native share or app chooser appears, at least one
receiving app is offered, and the selected app receives the expected text, URL,
file, or manifest-style mixed payload.

The CLI example is useful for desktop payload checks, but it is not a
substitute for the Makepad test app. Some platforms require an active app
window, activity, or view controller to present a share sheet.

## Mobile Test Cases

Exercise the same logical payloads from the Makepad test app:

- `text`: `ShareSheet::new().add_text(...).share()`
- `url`: `ShareSheet::new().add_url(...).share()`
- `file`: one text or image attachment
- `mixed`: text + URL + one attachment
- `picked file`: one attachment selected through `robius-file-picker`

Android's picked-file case uses `add_file_uri()` with a `content://` URI that
the app can grant to the receiver. Android generated-file cases use `add_file()`;
on Android 10 and newer, those files are copied to a shareable MediaStore item
before launching the chooser. On Android 8 and 9, the generated-file example is
a small text file and is shared as text content.

iOS can use `add_file()` with a temporary or bundled local file.

## Linux-Specific Checks

Linux should be tested in two environments:

- Portal available: a Flatpak/Snap-style or desktop session with
  `org.freedesktop.portal.Desktop`.
- Portal unavailable: a minimal desktop session where the fallback path uses
  `xdg-open`.

The required Linux behavior is not a universal "share sheet"; it is an app
chooser/default-handler flow. URI payloads should go through `OpenURI` when
possible, and local files should go through `OpenFile` when possible.

## Minimum Release Matrix

For a normal release, the high-value matrix is:

- Automated gate once.
- One manual test pass on each target OS family: Android, iOS, macOS, Windows,
  Linux with portal.
- One Linux fallback test pass without portal, if Linux behavior changed.
- One repeated-share check on Windows and Android, if lifecycle or concurrency
  code changed.

That reduces the native matrix to the behaviors most likely to regress while
still covering every platform.
