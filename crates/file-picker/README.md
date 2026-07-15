# `robius-file-picker`

Rust abstractions for native file picker dialogs, including dedicated image and video pickers on mobile.

## Platform behavior:
* macOS, Linux, Windows: desktop platforms are effectively a thin wrapper aroun [`rfd`](https://crates.io/crates/rfd).
* Android: file pickers use the system document picker (via `ACTION_OPEN_DOCUMENT`/`ACTION_CREATE_DOCUMENT`)
   * Android media picking uses the system photo picker when available, falling back to the above doc picker.
   * On Android 10 and up, it can save directly to "Downloads" via `MediaStore.Downloads`.
   * On Android 8 an 9, it writes directly to storage when the legacy external storage write permission
     is granted, and otherwise falls back to `ACTION_CREATE_DOCUMENT`.
   * **Minimum API level: 26 (Android 8.0).** The bundled Java helper is loaded via `InMemoryDexClassLoader`,
     which requires API 26. Newer features (the system Photo Picker, `MediaStore.Downloads`) are used only
     when the device supports them, so set `minSdk` to at least 26.
* iOS: file operations use `UIDocumentPickerViewController`, media picking uses`PHPickerViewController`.

All completion callbacks run on a background thread (a native OS thread, not an async task).
We never run the picker dialog popup on the main UI thread, so they are can easily do blocking ops
(e.g., reading the file that was picked, copying its bytes around, etc) without freezing the UI.

You can use your UI toolkit's inter-thread communication primitives (like `Cx::post_action()` in Makepad)
to deliver the results from the callback that runs on background thread to your main UI thread.

## Examples

```rust
use robius_file_picker::FileDialog;

FileDialog::new()
    .add_filter("Text", &["txt", "md"])
    .pick_file(|result| {
        if let Ok(Some(file)) = result {
            if let Some(path) = file.path() {
                println!("selected path: {path:?}");
            } else if let Some(uri) = file.uri() {
                println!("selected uri: {uri}");
            }
        }
    })
    .expect("failed to show file dialog");
```

On Android, the picked files are returned as `content://` URIs (there's no other option),
and regular fs paths on all other platforms (desktop & iOS). 
This crate offers an abstraction over that via `File::into_local_file()`, which either returns
the existing real fs path or streams the URI's content into a temp file in the app's cache dir,
so you are always able to get the files as a regular path.

```rust
use robius_file_picker::FileDialog;

FileDialog::new()
    .pick_file(|result| {
        if let Ok(Some(file)) = result {
            // Runs on a background thread; the URI is streamed to storage.
            let local = file.into_local_file().expect("failed");
            println!("local path: {:?}", local.path());
            // The `local` binding keeps the temp file copy alive.
            // Drop it when you're done with it to clean up that temp file.
        }
    })
    .expect("failed to show file dialog");
```

You can also pick an image or video using the platform's native media picker,
or restrict it to just images or just videos via `pick_image` and `pick_video`, respectively.

```rust
use robius_file_picker::FileDialog;

FileDialog::new()
    .pick_image_or_video(|result| {
        if let Ok(Some(file)) = result {
            println!("selected media: {file:?}");
        }
    })
    .expect("failed to show media picker");
```

By default, the media pickers filter using a set of common extensions;
see `DEFAULT_IMAGE_EXTENSIONS` and `DEFAULT_VIDEO_EXTENSIONS`.
Setting any filter of your own fully overrides those defaults.
Use `add_filter` to append filter groups one at a time, or `set_filters` to replace the entire set at once (in which passing an empty set will clear all filters). For example, to allow only `.tiff` files:

```rust
use robius_file_picker::FileDialog;

FileDialog::new()
    .add_filter("TIFF", &["tiff"])
    .pick_image(|result| {
        if let Ok(Some(file)) = result {
            println!("selected image: {file:?}");
        }
    })
    .expect("failed to show image picker");
```

Important: a filter only narrows the selectable files if the native picker supports it.
Desktop file dialogs do support it, but mobile image/video pickers won't do any restrictions beyond the general category of "image" or "video".

Note that this crate does not perform any filtering after the picker returns, all filtering is done by the native OS-provider picker. 
If you want additional filtering of file/mime types, you should do it in your app.


```rust
use robius_file_picker::FileDialog;

let bytes: Vec<u8> = b"report contents".to_vec();
FileDialog::new()
    .set_file_name("report.pdf")
    .set_mime_type("application/pdf")
    .save_data(
        bytes,
        |result| {
            if let Ok(Some(file)) = result {
                println!("saved to: {file:?}");
            }
        }
    )
    .expect("failed to show save dialog");
```

The `save_data` function is a clean abstraction that behaves differently under the hood on each platform:
* Desktop: it writes data directly to the chosen fs path
* Android: it writes data to the `content://` URI on Android
* iOS: write the bytes to a temporary file which is then exported by the system document picker.
   * This design is out of necessity and due to iOS's limitations, there's really not much else we can do there.

You don't really need to worry about those details, they're just here to explain why the function signature is the way it is. 

Finally, there's a convenience function for saving bytes directly to the user's Downloads directory.
Note that on iOS, `save_to_downloads` falls back to the system document export picker because there is no real Downloads dir.

```rust
use robius_file_picker::FileDialog;

FileDialog::new()
    .set_file_name("image.png")
    .set_mime_type("image/png")
    .save_to_downloads("/path/to/image.png", |result| {
        if let Ok(Some(file)) = result {
            println!("saved to Downloads: {file:?}");
        }
    })
    .expect("failed to save to Downloads");
```

