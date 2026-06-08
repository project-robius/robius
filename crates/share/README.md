# robius-share

`robius-share` provides a Rust builder API for showing the native system share
sheet from an app.

```rust
use robius_share::ShareSheet;

ShareSheet::new()
    .set_title("Share")
    .set_subject("Robius")
    .add_text("Cross-platform native APIs from Rust")
    .add_url("https://robius.rs/")
    .share()?;
# Ok::<(), robius_share::Error>(())
```

## Platform behavior

- Android uses `Intent.ACTION_SEND` / `ACTION_SEND_MULTIPLE` through
  `Intent.createChooser`.
- iOS uses `UIActivityViewController`.
- macOS uses `NSSharingServicePicker`.
- Windows uses the native WinRT Share UI through desktop-window interop.
- Linux uses the XDG desktop portal app chooser when available, calling
  `OpenURI` for URI payloads and `OpenFile` for local files, then falling back
  to `xdg-open`. For text-only or mixed payloads that cannot be represented as
  one URI or file, it writes a temporary text payload and opens that with the
  platform app chooser/default handler.

Android file attachments must be `content://` URIs supplied with
`add_file_uri()` or `add_file_uri_with_mime_type()`. A Rust library cannot safely
share arbitrary private filesystem paths on Android without a manifest-registered
`ContentProvider`.

See [TESTING.md](TESTING.md) for the automated checks and native test cases
used to validate this crate across platforms.
