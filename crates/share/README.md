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

Android `content://` attachments are shared directly. Filesystem path
attachments are copied to a shareable MediaStore item on Android 10 and newer.
On Android 8 and 9, API 26 through 28, text files can be shared as text
content; arbitrary binary path attachments require a host-app `ContentProvider`.

The **minimum supported Android API level is 26 (Android 8.0)**: the bundled Java
helper is loaded via `InMemoryDexClassLoader`, which requires API 26, so set
`minSdk` to at least 26 in your app.

## Android file attachments

Android receivers should be given `content://` URIs, not private app filesystem
paths. A `ContentProvider` is the Android component that turns an app-private
file into a URI another app can read temporarily. A provider authority is the
unique name Android uses to route that URI back to the app that owns the
provider. For example, an app whose package id is `dev.example.notes` might use:

```text
dev.example.notes.robius.share
```

That authority must be unique per installed app. `robius-share` cannot use one
global authority like `robius.share`, because two different apps that depend on
this crate would then declare the same provider authority and conflict at
install time. The authority also has to appear in the final Android manifest;
it cannot be registered after the app starts.

This provider setup is only required by `robius-share` when all of these are
true:

- the app is running on Android 8 or 9, API 26 through 28;
- the payload is an app-private filesystem path;
- the file must be shared as a real attachment rather than as text content;
- the file is an arbitrary binary file, such as a PDF, image, video, archive, or
  custom document.

It is not needed for `content://` URIs returned by `robius-file-picker`, because
those URIs already come from a provider. It is also not needed for
`add_file(path)` on Android 10 and newer, because `robius-share` copies the file
to a shareable MediaStore item before opening the Android Sharesheet. If your
app already has a `FileProvider` or custom `ContentProvider`, you can reuse that
instead of adding a separate Robius-specific provider.

The normal app integration is:

```xml
<provider
    android:name="androidx.core.content.FileProvider"
    android:authorities="${applicationId}.robius.share"
    android:exported="false"
    android:grantUriPermissions="true">
    <meta-data
        android:name="android.support.FILE_PROVIDER_PATHS"
        android:resource="@xml/robius_share_paths" />
</provider>
```

with a matching `res/xml/robius_share_paths.xml`, for example:

```xml
<paths>
    <cache-path name="robius-share-cache" path="robius-share/" />
    <files-path name="robius-share-files" path="robius-share/" />
</paths>
```

For Makepad apps, the same idea can be expressed in
`resources/android/AndroidManifest.xml.template` using Makepad's package token:

```xml
android:authorities="{package_id}.robius.share"
```

Once your app has converted a file path into a provider-backed `content://` URI,
pass that URI to `robius-share`:

```rust
use robius_share::ShareSheet;

let uri = "content://dev.example.notes.robius.share/robius-share-cache/report.pdf";

ShareSheet::new()
    .set_title("Share report")
    .add_file_uri_with_mime_type(uri, "application/pdf")
    .share()?;
# Ok::<(), robius_share::Error>(())
```

Use `add_file(path)` when you have a normal filesystem path and the current
platform can make that path shareable:

```rust
use std::{env, fs};
use robius_share::ShareSheet;

let path = env::temp_dir().join("robius-share-report.txt");
fs::write(&path, "Generated report\n")?;

ShareSheet::new()
    .set_title("Share report")
    .add_file_with_mime_type(&path, "text/plain")
    .share()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

That `add_file(...)` example is a true file attachment on iOS, macOS, Windows,
Linux, and Android 10 or newer. On Android 8 and 9, the crate can share small
text files as text content, but arbitrary binary paths such as PDFs, images, or
videos need the host app to provide a `content://` URI and call `add_file_uri`.

Android's FileProvider setup is documented here:
<https://developer.android.com/training/secure-file-sharing/setup-sharing>.
Android's MediaStore behavior is documented here:
<https://developer.android.com/training/data-storage/shared/media>.

See [TESTING.md](TESTING.md) for the automated checks and native test cases
used to validate this crate across platforms.
