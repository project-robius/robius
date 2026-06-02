//! Multi-platform abstractions for native file picker dialogs,
//! including dedicated image and video pickers on mobile.
//!
//! ## Platform behavior
//! All types & functions in this crate are completely platform-independent,
//! so your app code doesn't need to deal with any of this, but here are some
//! details about how things are implemented on a per-platform basis, in case you're curious.
//! * **macOS**, **Linux**, **Windows**: desktop platforms are mostly a wrapper around [`rfd`].
//! * **Android**: file pickers use the system document picker
//!   (via//!   `ACTION_OPEN_DOCUMENT`/`ACTION_CREATE_DOCUMENT`).
//!   * Media picking uses the system Photo Picker when available,
//!     otherwise it falls back to the document picker above.
//!   * On Android 10 and up, it saves directly to Downloads via `MediaStore.Downloads`.
//!   * On Android 8 and 9, it writes directly to storage if the legacy external storage
//!     write permission is granted. Otherwise it falls back to `ACTION_CREATE_DOCUMENT`.
//! * **iOS**: file picking use [`UIDocumentPickerViewController`], while 
//!   media picking uses `PHPickerViewController`.
//!
//! On Android, picked files are returned as `content://` URIs (there's no other real option),
//! and as regular filesystem paths on all other platforms (desktop and iOS).
//! The [`PickedFile::into_local_file`] function abstracts over that by returning either the
//! existing real filesystem path or streaming the URI's content into a temp file
//! in the app's cache dir and then returning that.
//! Thus, you can always get the file as a regular path.
//!
//! ## Callbacks and thread contexts
//!
//! All completion callbacks run on a background thread (a true native OS thread,
//! not an async task). The picker dialog popup never runs on the main UI thread,
//! so callbacks can easily do blocking work like reading the picked file or copying
//! its bytes around without freezing the UI.
//!
//! To communicate between the callback that runs in a background thread and your app's
//! main UI thread, use a communication primitive like a channel, or something similar
//! from your UI toolkit, e.g., `Cx::post_action` in Makepad.
//!
//! [`rfd`]: https://crates.io/crates/rfd
//! [`UIDocumentPickerViewController`]: https://developer.apple.com/documentation/uikit/uidocumentpickerviewcontroller

mod error;
mod sys;

use std::{path::{Path, PathBuf}, sync::Arc};

pub use error::{Error, Result};
pub(crate) type DialogCallback = Box<dyn FnOnce(Result<Option<PickedFile>>) + Send + 'static>;
pub(crate) type DialogData = Box<dyn AsRef<[u8]> + Send + 'static>;


/// The image file extensions that [`pick_image`](FileDialog::pick_image) (and
/// the image half of [`pick_image_or_video`](FileDialog::pick_image_or_video))
/// filter by, unless you override them with [`FileDialog::add_filter`].
///
/// This default set only narrows the selection on platforms whose native picker
/// filters by extension (i.e. the desktop file dialogs). The mobile media
/// pickers filter by the broad "images" category instead, so they accept any
/// image type regardless of this list.
pub const DEFAULT_IMAGE_EXTENSIONS: &[&str] = &[
    // Common display/web formats.
    "jpg", "jpeg", "jfif", "png", "apng", "gif", "bmp", "dib", "webp", "avif", "heic", "heif",
    "tif", "tiff", "svg", "svgz", "ico", "jp2", "jxl", "pbm", "pgm", "ppm", "tga", "psd",
    // Common camera RAW formats.
    "raw", "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef",
    "srw", "dcr", "x3f",
];

/// The video file extensions that [`pick_video`](FileDialog::pick_video) (and
/// the video half of [`pick_image_or_video`](FileDialog::pick_image_or_video))
/// filter by, unless you override them with [`FileDialog::add_filter`].
///
/// This default set only narrows the selection on platforms whose native picker
/// filters by extension (i.e. the desktop file dialogs). The mobile media
/// pickers filter by the broad "videos" category instead, so they accept any
/// video type regardless of this list.
pub const DEFAULT_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "qt", "avi", "mkv", "webm", "wmv", "asf", "flv", "f4v", "ogv", "ogm",
    "mpg", "mpeg", "mpe", "m2v", "mp2", "mts", "m2ts", "ts", "3gp", "3g2", "mxf", "vob", "rm",
    "rmvb", "divx", "m4p",
];

/// A native file dialog builder.
#[derive(Clone, Debug, Default)]
pub struct FileDialog {
    options: DialogOptions,
}

impl FileDialog {
    /// Creates a new file dialog builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a single named file extension filter, leaving existing filters intact.
    /// 
    /// Upon creation, a `FileDialog` has no filters. Adding any filter manually
    /// will result in the default image or video extension filters *not* being used.
    ///
    /// Extensions may be supplied with or without a leading dot '.'.
    /// To replace the whole filter set, use [`set_filters`](Self::set_filters) instead.
    ///
    /// See the README or top-level docs for more info.
    #[must_use]
    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        self.options.filters.push(FileFilter {
            name: name.into(),
            extensions: extensions.iter().map(|ext| ext.to_string()).collect(),
        });
        self
    }

    /// Replaces every previously added filter with the given set of `filters`.
    ///
    /// Passing an empty set clears all filters, including those previously added via `add_filter()`.
    ///
    /// ```norun
    /// # use robius_file_picker::FileDialog;
    /// let dialog = FileDialog::new().set_filters([
    ///     ("Images", &["png", "jpg"]),
    ///     ("Documents", &["pdf", "txt"]),
    /// ]);
    /// ```
    #[must_use]
    pub fn set_filters<N, X, E>(mut self, filters: impl IntoIterator<Item = (N, X)>) -> Self
    where
        N: Into<String>,
        X: IntoIterator<Item = E>,
        E: ToString,
    {
        self.options.filters = filters
            .into_iter()
            .map(|(name, extensions)| FileFilter {
                name: name.into(),
                extensions: extensions.into_iter().map(|ext| ext.to_string()).collect(),
            })
            .collect();
        self
    }

    /// Sets a platform MIME type hint.
    ///
    /// This is primarily used on Android and iOS. Desktop backends might ignore it.
    #[must_use]
    pub fn set_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.options.mime_type = Some(mime_type.into());
        self
    }

    /// Sets the initial directory, where supported.
    #[must_use]
    pub fn set_directory<P: AsRef<Path>>(mut self, path: P) -> Self {
        let path = path.as_ref();
        if path.to_str().map(|p| p.is_empty()).unwrap_or(false) {
            self.options.directory = None;
        } else {
            self.options.directory = Some(path.to_owned());
        }
        self
    }

    /// Hints which well-known folder the dialog should open in.
    ///
    /// This is merely a hint; the OS might very well open the last-used location instead.
    #[must_use]
    pub fn set_start_location(mut self, location: StartLocation) -> Self {
        self.options.start_location = Some(location);
        self
    }

    /// Sets the suggested file name, where supported.
    #[must_use]
    pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.options.file_name = Some(file_name.into());
        self
    }

    /// Sets the dialog title, where supported.
    #[must_use]
    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.options.title = Some(title.into());
        self
    }

    /// Shows a native open-file dialog.
    ///
    /// The callback is called with `Ok(None)` if the user cancels the dialog.
    /// Returns [`Error::AlreadyOpen`] if the active native UI context is already
    /// presenting a picker.
    pub fn pick_file<F>(self, on_completion: F) -> Result<()>
    where
        F: FnOnce(Result<Option<PickedFile>>) + Send + 'static,
    {
        sys::pick_file(self.options, Box::new(on_completion))
    }

    /// Shows the platform's native image picker.
    ///
    /// The callback is called with `Ok(None)` if the user cancels the picker.
    pub fn pick_image<F>(self, on_completion: F) -> Result<()>
    where
        F: FnOnce(Result<Option<PickedFile>>) + Send + 'static,
    {
        self.pick_media(MediaKind::Image, on_completion)
    }

    /// Shows the platform's native video picker.
    ///
    /// The callback is called with `Ok(None)` if the user cancels the picker.
    pub fn pick_video<F>(self, on_completion: F) -> Result<()>
    where
        F: FnOnce(Result<Option<PickedFile>>) + Send + 'static,
    {
        self.pick_media(MediaKind::Video, on_completion)
    }

    /// Shows the platform's native image and video picker.
    ///
    /// The callback is called with `Ok(None)` if the user cancels the picker.
    pub fn pick_image_or_video<F>(self, on_completion: F) -> Result<()>
    where
        F: FnOnce(Result<Option<PickedFile>>) + Send + 'static,
    {
        self.pick_media(MediaKind::ImageOrVideo, on_completion)
    }

    /// Shows the platform's native media picker.
    ///
    /// * Android: shows the Android Photo Picker, falling back to the document picker
    ///   with a filter for images/videos.
    /// * iOS: shows the `PHPickerViewController`.
    /// * Desktop: shows a filtered file dialog.
    ///
    /// With no explicit location set, defaults the start location to Pictures or
    /// Videos, which only the desktop dialog and document-picker fallbacks use.
    ///
    /// Returns [`Error::AlreadyOpen`] if the active native UI context is already
    /// presenting a picker.
    pub fn pick_media<F>(mut self, media_kind: MediaKind, on_completion: F) -> Result<()>
    where
        F: FnOnce(Result<Option<PickedFile>>) + Send + 'static,
    {
        if self.options.directory.is_none() && self.options.start_location.is_none() {
            self.options.start_location = Some(match media_kind {
                MediaKind::Image | MediaKind::ImageOrVideo => StartLocation::Pictures,
                MediaKind::Video => StartLocation::Videos,
            });
        }
        sys::pick_media(self.options, media_kind, Box::new(on_completion))
    }

    /// Saves the given `data` bytes to a user-specific location via platform-native operations.
    ///
    /// You should first set a file name via [`FileDialog::set_file_name`],
    /// otherwise this will return [`Error::InvalidFileName`].
    /// Returns [`Error::AlreadyOpen`] if the active native UI context is already
    /// presenting a picker.
    pub fn save_data<D, F>(self, data: D, on_completion: F) -> Result<()>
    where
        D: AsRef<[u8]> + Send + 'static,
        F: FnOnce(Result<Option<PickedFile>>) + Send + 'static,
    {
        sys::save_data(self.options, Box::new(data), Box::new(on_completion))
    }

    /// Saves a file to the user's Downloads location.
    pub fn save_to_downloads<P, F>(self, source_path: P, on_completion: F) -> Result<()>
    where
        P: AsRef<Path>,
        F: FnOnce(Result<Option<PickedFile>>) + Send + 'static,
    {
        sys::save_to_downloads(
            self.options,
            source_path.as_ref().to_owned(),
            Box::new(on_completion),
        )
    }
}

/// The type of media to show in a native media picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    #[doc(alias("photo", "picture"))]
    Image,
    #[doc(alias = "movie")]
    Video,
    #[doc(alias("photos", "pictures", "movies"))]
    ImageOrVideo,
}

/// A well-known directory that a file dialog should start in.
///
/// Used with [`FileDialog::set_start_location`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartLocation {
    Desktop,
    Documents,
    Downloads,
    #[doc(alias("photo", "image"))]
    Pictures,
    #[doc(alias = "audio")]
    Music,
    #[doc(alias = "movie")]
    Videos,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileFilter {
    pub(crate) name: String,
    pub(crate) extensions: Vec<String>,
}

/// Options collected by [`FileDialog`].
#[derive(Clone, Debug, Default)]
pub(crate) struct DialogOptions {
    pub(crate) filters: Vec<FileFilter>,
    pub(crate) directory: Option<PathBuf>,
    pub(crate) start_location: Option<StartLocation>,
    pub(crate) file_name: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) mime_type: Option<String>,
}

impl DialogOptions {
    pub(crate) fn output_file_name(&self, source_path: &Path) -> Result<String> {
        self.file_name
            .as_deref()
            .and_then(file_name_component)
            .or_else(|| {
                source_path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
            })
            .filter(|file_name| !file_name.is_empty())
            .map(ToOwned::to_owned)
            .ok_or(Error::InvalidFileName)
    }

    pub(crate) fn output_file_name_only(&self) -> Result<String> {
        self.file_name
            .as_deref()
            .and_then(file_name_component)
            .filter(|file_name| !file_name.is_empty())
            .map(ToOwned::to_owned)
            .ok_or(Error::InvalidFileName)
    }
}

fn file_name_component(file_name: &str) -> Option<&str> {
    Path::new(file_name)
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .filter(|file_name| !file_name.is_empty())
}

/// A file chosen by the user in a native file/media picker dialog.
///
/// Under the hood, this is either a filesystem path or a `content://` URI,
/// plus any metadata that the platform provided (name, MIME type, size).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickedFile {
    location: FileLocation,
    display_name: Option<String>,
    mime_type: Option<String>,
    size: Option<u64>,
}

impl PickedFile {
    #[allow(dead_code)]
    pub(crate) fn from_path(path: PathBuf) -> Self {
        Self {
            location: FileLocation::Path {
                path,
                owned_temp: false,
            },
            display_name: None,
            mime_type: None,
            size: None,
        }
    }

    /// Like [`PickedFile::from_path`], but for a temporary file created by this library.
    #[allow(dead_code)]
    pub(crate) fn from_owned_temp_path(path: PathBuf) -> Self {
        Self {
            location: FileLocation::Path {
                path,
                owned_temp: true,
            },
            display_name: None,
            mime_type: None,
            size: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn from_uri(uri: String) -> Self {
        Self {
            location: FileLocation::Uri(uri),
            display_name: None,
            mime_type: None,
            size: None,
        }
    }

    /// Builds a URI-backed file with metadata (mostly for Android).
    #[allow(dead_code)]
    pub(crate) fn from_uri_with_metadata(
        uri: String,
        display_name: Option<String>,
        mime_type: Option<String>,
        size: Option<u64>,
    ) -> Self {
        Self {
            location: FileLocation::Uri(uri),
            display_name: display_name.filter(|s| !s.is_empty()),
            mime_type: mime_type.filter(|s| !s.is_empty()),
            size,
        }
    }

    /// Returns this file as a filesystem path, if the platform returned one.
    pub fn path(&self) -> Option<&Path> {
        match &self.location {
            FileLocation::Path { path, .. } => Some(path),
            FileLocation::Uri(_) => None,
        }
    }

    /// Returns this file as a platform URI, if the platform returned one.
    pub fn uri(&self) -> Option<&str> {
        match &self.location {
            FileLocation::Path { .. } => None,
            FileLocation::Uri(uri) => Some(uri),
        }
    }

    /// Consumes this file and returns its filesystem path, if any.
    pub fn into_path(self) -> Option<PathBuf> {
        match self.location {
            FileLocation::Path { path, .. } => Some(path),
            FileLocation::Uri(_) => None,
        }
    }

    /// Consumes this file and returns its platform URI, if any.
    pub fn into_uri(self) -> Option<String> {
        match self.location {
            FileLocation::Path { .. } => None,
            FileLocation::Uri(uri) => Some(uri),
        }
    }

    /// The platform-provided display name, if known (Android).
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// The platform-provided MIME type, if known (Android).
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }

    /// The platform-provided size in bytes, if known (Android).
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    /// Returns a best-effort file name: the platform-provided display name,
    /// otherwise the final path component or URI segment.
    pub fn file_name(&self) -> Option<&str> {
        if let Some(name) = self.display_name.as_deref().filter(|s| !s.is_empty()) {
            return Some(name);
        }
        match &self.location {
            FileLocation::Path { path, .. } => path.file_name().and_then(|name| name.to_str()),
            FileLocation::Uri(uri) => uri
                .split('?')
                .next()
                .unwrap_or(uri)
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty()),
        }
    }

    /// Reads the file's contents into memory.
    ///
    /// This buffers the whole file; generally you should use [`PickedFile::into_local_file`]
    /// if you need to write it to storage instead.
    pub fn read_bytes(&self) -> Result<Vec<u8>> {
        match &self.location {
            FileLocation::Path { path, .. } => Ok(std::fs::read(path)?),
            FileLocation::Uri(uri) => sys::read_uri_bytes(uri),
        }
    }

    /// Allows accessing this file via a real filesystem path.
    ///
    /// A path-backed file (desktop, iOS) is returned as-is.
    /// A URI-backed file (Android) is streamed to a temp file in the app's cache dir,
    /// which avoids buffering it in memory (nice!).
    ///
    /// Returns a [`LocalFile`], which owns any temp file instance and deletes it
    /// once the last reference is dropped.
    ///
    /// This performs blocking I/O, so don't call it on the main UI thread.
    /// Calling it in a callback is totally fine, those already run on background threads.
    pub fn into_local_file(self) -> Result<LocalFile> {

        fn uri_file_name(uri: &str) -> Option<&str> {
            uri.split('?')
                .next()
                .unwrap_or(uri)
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
        }

        let PickedFile {
            location,
            display_name,
            mime_type,
            size,
        } = self;
        match location {
            FileLocation::Path { path, owned_temp } => Ok(LocalFile {
                cleanup: owned_temp.then(|| Arc::new(TempPathCleanup { path: path.clone() })),
                path,
                display_name,
                mime_type,
                size,
            }),
            FileLocation::Uri(uri) => {
                let file_name = display_name
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .or_else(|| uri_file_name(&uri))
                    .unwrap_or("attachment")
                    .to_owned();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let dir = sys::app_temp_dir()?
                    .join(format!("robius-file-picker-{}-{now}", std::process::id()));
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(file_name);
                // Stream the URI's contents straight to disk on the backend.
                sys::copy_uri_to_path(&uri, &path)?;
                Ok(LocalFile {
                    cleanup: Some(Arc::new(TempPathCleanup { path: path.clone() })),
                    path,
                    display_name,
                    mime_type,
                    size,
                })
            }
        }
    }
}


/// A file guaranteed to be reachable through a filesystem path.
///
/// Produced by [`PickedFile::into_local_file`], and as stated there,
/// this auto-handles cleaning up any temp file instances it has created.
///
/// This instance should live for as long as you need to access/use the file,
/// for example, throughout the duration of an upload/save operation.
#[derive(Clone, Debug)]
pub struct LocalFile {
    path: PathBuf,
    display_name: Option<String>,
    mime_type: Option<String>,
    size: Option<u64>,
    // If `Some`, there is a temp file to clean up.
    // If `None, the `path` points to the original real file and shouldn't be touched.
    cleanup: Option<Arc<TempPathCleanup>>,
}

impl LocalFile {
    /// The local filesystem path to the file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The platform-provided display name, if known (Android).
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// The platform-provided MIME type, if known (Android).
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }

    /// The platform-provided size in bytes, if known (Android).
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    /// Whether this path is a temporary copy that will be cleaned up on drop.
    pub fn is_temporary(&self) -> bool {
        self.cleanup.is_some()
    }
}

#[derive(Debug)]
struct TempPathCleanup {
    path: PathBuf,
}

impl Drop for TempPathCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        // Remove the enclosing directory too, if it's empty.
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// The platform-specific location of a picked file.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum FileLocation {
    Path {
        path: PathBuf,
        /// If true, this referes to a temp file that we should clean up later.
        owned_temp: bool,
    },
    Uri(String),
}
