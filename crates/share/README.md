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
On Android 8 and 9, API 26 through 28, they are copied into the app's cache and
shared through the host app's `ContentProvider`, if it declares one (see
below). Without one, text files are shared as text content, and arbitrary binary
path attachments can't be shared.

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
it cannot be registered after the app starts, so the host app has to declare
the provider.

A provider is only needed on Android 8 and 9, API 26 through 28, to share
filesystem path attachments (`add_file(path)`). It is not needed for
`content://` URIs, such as those returned by `robius-file-picker`, because those
URIs already come from a provider. It is also not needed for `add_file(path)` on
Android 10 and newer, because `robius-share` copies the file to a shareable
MediaStore item before opening the Android Sharesheet.

On Android 8 and 9, `add_file(path)` uses the provider whose authority is the
app's package name followed by `.robius.share`, if the app declares one:

- it copies the file to `<cache dir>/robius-share/<uuid>/<file name>`, where
  `<cache dir>` is the app's `Context.getCacheDir()` and `<uuid>` is a new
  random UUID, in lowercase;
- it shares that copy as
  `content://<package name>.robius.share/robius-share/<uuid>/<file name>`,
  so the provider has to serve the `robius-share` directory of the cache under
  the URI path `robius-share`;
- it only uses a provider that belongs to the app itself and lets it grant
  access to that URI (`android:grantUriPermissions="true"`, or a matching
  `<grant-uri-permission>`), and it checks that the provider serves the copy.
  Otherwise it falls back to the behavior without a provider;
- on a later share, it deletes the directories in `<cache dir>/robius-share`
  whose names are lowercase UUIDs and that haven't changed for a day, without
  following symbolic links. The app may keep its own files in that directory,
  as long as their names aren't UUIDs.

With androidx's `FileProvider`, that's:

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

with a matching `res/xml/robius_share_paths.xml`:

```xml
<paths>
    <!-- The name must be "robius-share", the URI path robius-share uses. -->
    <cache-path name="robius-share" path="robius-share/" />
</paths>
```

Makepad apps don't bundle androidx, so they can't use `FileProvider`. cargo-makepad
builds that include `dev.makepad.android.MakepadFileProvider` can declare that
instead, in `resources/android/AndroidManifest.xml.template`, using Makepad's
package token:

```xml
<provider
    android:name="dev.makepad.android.MakepadFileProvider"
    android:authorities="{package_id}.robius.share"
    android:exported="false"
    android:grantUriPermissions="true">
    <meta-data
        android:name="dev.makepad.android.MakepadFileProvider.cacheDirs"
        android:value="robius-share" />
</provider>
```

Only declare a provider class that your APK actually contains: Android starts
an app's providers when the app starts, so a missing class crashes the app at
launch on every Android version.

An app can also make its own `content://` URIs, with this provider or any other
one it already has, and pass them to `robius-share`:

```rust
use robius_share::ShareSheet;

let uri = "content://dev.example.notes.robius.share/robius-share/report.pdf";

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
Linux, and Android 10 or newer. On Android 8 and 9 it is one only if the app
declares the provider described above; without one, small text files are shared
as text content, and arbitrary binary paths such as PDFs, images, or videos
can't be shared.

Android's FileProvider setup is documented here:
<https://developer.android.com/training/secure-file-sharing/setup-sharing>.
Android's MediaStore behavior is documented here:
<https://developer.android.com/training/data-storage/shared/media>.
