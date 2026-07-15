//! Multi-platform abstractions for showing the system-native share sheet.
//!
//! ## Platform behavior
//! All types and functions in this crate are platform-independent, but native
//! share APIs don't expose exactly the same payload model on every OS:
//! * **Android**: sharing uses `Intent.ACTION_SEND` or `ACTION_SEND_MULTIPLE`,
//!   wrapped in the Android Sharesheet via `Intent.createChooser`.
//!   * Text, URLs, `content://` attachments, and filesystem path attachments
//!     are supported.
//!   * Filesystem path attachments are copied to a shareable MediaStore item on
//!     Android 10 and newer. On older Android versions, text files can be shared
//!     as text content, but arbitrary binary path attachments require an app
//!     `ContentProvider`.
//!   * **Minimum API level: 26 (Android 8.0).** The bundled Java helper is loaded
//!     via `InMemoryDexClassLoader`, which requires API 26. Set `minSdk` to at
//!     least 26 in your app.
//! * **iOS**: sharing uses `UIActivityViewController`, with text, URLs, and
//!   filesystem path attachments.
//! * **macOS**: sharing uses `NSSharingServicePicker`, with text, URLs, and
//!   filesystem path attachments.
//! * **Windows**: sharing uses the native WinRT Share UI through
//!   `DataTransferManager` desktop-window interop, with text, URLs, and
//!   filesystem path attachments.
//! * **Linux**: Linux doesn't have a single standardized share sheet, so this
//!   crate talks to the XDG desktop portal over a minimal D-Bus interface.
//!   * A single file or URL is shared via the "Open With" app chooser.
//!   * A mixed payload (multiple items) will open a save files dialog,
//!     and write each item to the directory chosen by the user.
//!   * If the d-bus portal doesn't work or isn't available, it falls back to `xdg-open`.
//!
//! ## Completion
//! * On macOS, iOS, and Windows, a successful `share()` means the native share sheet
//!   was presented.
//! * On Android and Linux, the share request is fire-and-forget, so a successful
//!   return value means that the request was dispatched, not that anything was shown.
//!
//! ## Examples
//!
//! ```no_run
//! use robius_share::ShareSheet;
//!
//! ShareSheet::new()
//!     .set_title("Share")
//!     .set_subject("Robius")
//!     .add_text("Cross-platform native APIs from Rust")
//!     .add_url("https://robius.rs/")
//!     .share()
//!     .expect("failed to show share sheet");
//! ```

mod error;
mod sys;

use std::path::Path;

pub use error::{Error, Result};
use robius_common::FileLocation;

/// A native share sheet builder.
#[derive(Clone, Debug, Default)]
pub struct ShareSheet {
    options: ShareOptions,
}

impl ShareSheet {
    /// Creates a new share sheet builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a share sheet title or chooser prompt, where supported.
    ///
    /// On Android this is used as the chooser title. On iOS/macOS, the system
    /// share sheet might ignore it.
    #[must_use]
    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.options.title = Some(title.into());
        self
    }

    /// Sets secondary share metadata for receivers that support it.
    ///
    /// Some share targets use this as a subject, description, or preview title.
    /// Many targets ignore it.
    #[must_use]
    pub fn set_subject(mut self, subject: impl Into<String>) -> Self {
        self.options.subject = Some(subject.into());
        self
    }

    /// Adds plain text to the share payload.
    #[must_use]
    pub fn add_text(mut self, text: impl Into<String>) -> Self {
        self.options.items.push(ShareItem::Text(text.into()));
        self
    }

    /// Adds a web or app URL to the share payload.
    ///
    /// On Android this is encoded as shared text, because `ACTION_SEND` has no
    /// separate URL field.
    #[must_use]
    pub fn add_url(mut self, url: impl Into<String>) -> Self {
        self.options.items.push(ShareItem::Url(url.into()));
        self
    }

    /// Adds a filesystem path attachment.
    ///
    /// On Android 10 and newer, this copies the file to a shareable MediaStore
    /// item before launching the chooser. On older Android versions, text files
    /// can be shared as text content, while arbitrary binary path attachments
    /// require a host app `ContentProvider`.
    #[must_use]
    pub fn add_file<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.options.items.push(ShareItem::File(SharedFile::from_path(path)));
        self
    }

    /// Adds a filesystem path attachment with an explicit MIME type hint.
    #[must_use]
    pub fn add_file_with_mime_type<P: AsRef<Path>>(
        mut self,
        path: P,
        mime_type: impl Into<String>,
    ) -> Self {
        self.options.items.push(ShareItem::File(
            SharedFile::from_path(path).set_mime_type(mime_type),
        ));
        self
    }

    /// Adds a platform file/content URI attachment.
    ///
    /// This is primarily for Android `content://` URIs that your app already
    /// owns or received from another platform API. Local `file://` URIs are
    /// accepted on Android and are copied to a shareable MediaStore item where
    /// supported.
    #[must_use]
    pub fn add_file_uri(mut self, uri: impl Into<String>) -> Self {
        self.options.items.push(ShareItem::File(SharedFile::from_uri(uri)));
        self
    }

    /// Adds a platform file/content URI attachment with an explicit MIME type hint.
    #[must_use]
    pub fn add_file_uri_with_mime_type(
        mut self,
        uri: impl Into<String>,
        mime_type: impl Into<String>,
    ) -> Self {
        self.options.items.push(ShareItem::File(
            SharedFile::from_uri(uri).set_mime_type(mime_type),
        ));
        self
    }

    /// Shows the native share sheet.
    ///
    /// The returned result only indicates whether the share sheet was presented.
    /// It does not indicate whether the user chose a target or completed sharing.
    pub fn share(self) -> Result<()> {
        self.options.validate()?;
        sys::share(self.options)
    }
}

/// A file attachment in a share payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedFile {
    location: FileLocation,
    mime_type: Option<String>,
}

impl SharedFile {
    /// Creates a file attachment from a filesystem path.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Self {
        Self {
            location: FileLocation::from_path(path),
            mime_type: None,
        }
    }

    /// Creates a file attachment from a platform file/content URI.
    pub fn from_uri(uri: impl Into<String>) -> Self {
        Self {
            location: FileLocation::from_uri(uri),
            mime_type: None,
        }
    }

    /// Sets an explicit MIME type hint for this file attachment.
    #[must_use]
    pub fn set_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// Returns this attachment as a filesystem path, if it has one.
    pub fn path(&self) -> Option<&Path> {
        self.location.path()
    }

    /// Returns this attachment as a platform URI, if it has one.
    pub fn uri(&self) -> Option<&str> {
        self.location.uri()
    }

    /// Returns the explicit MIME type hint, if one was set.
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }
}

/// A single share payload item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareItem {
    /// Plain text content.
    Text(String),
    /// A URL to share as a link.
    Url(String),
    /// A file attachment.
    File(SharedFile),
}

/// Options collected by [`ShareSheet`].
#[derive(Clone, Debug, Default)]
pub(crate) struct ShareOptions {
    pub(crate) title: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) items: Vec<ShareItem>,
}

impl ShareOptions {
    fn validate(&self) -> Result<()> {
        if self.items.is_empty() {
            return Err(Error::Empty);
        }

        for item in &self.items {
            match item {
                ShareItem::Text(text) if text.trim().is_empty() => return Err(Error::InvalidItem),
                ShareItem::Url(url) if url.trim().is_empty() => return Err(Error::InvalidItem),
                ShareItem::File(file) => match &file.location {
                    FileLocation::Path(path) if path.as_os_str().is_empty() => {
                        return Err(Error::InvalidItem);
                    }
                    FileLocation::Uri(uri) if uri.trim().is_empty() => {
                        return Err(Error::InvalidItem);
                    }
                    _ if file.mime_type.as_deref().is_some_and(|m| m.trim().is_empty()) => {
                        return Err(Error::InvalidItem);
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok(())
    }
}

pub(crate) fn text_items(options: &ShareOptions) -> Vec<&str> {
    options
        .items
        .iter()
        .filter_map(|item| match item {
            ShareItem::Text(text) | ShareItem::Url(text) => Some(text.as_str()),
            ShareItem::File(_) => None,
        })
        .collect()
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn file_items(options: &ShareOptions) -> impl Iterator<Item = &SharedFile> {
    options.items.iter().filter_map(|item| match item {
        ShareItem::File(file) => Some(file),
        _ => None,
    })
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn shared_text(options: &ShareOptions) -> Option<String> {
    let mut text = String::new();
    for item in text_items(options) {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(item);
    }
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_payload_is_invalid() {
        assert!(matches!(
            ShareSheet::new().options.validate(),
            Err(Error::Empty)
        ));
    }

    #[test]
    fn empty_text_url_path_uri_and_mime_type_are_invalid() {
        let invalid_payloads = [
            ShareSheet::new().add_text(""),
            ShareSheet::new().add_url(""),
            ShareSheet::new().add_file(""),
            ShareSheet::new().add_file_uri(""),
            ShareSheet::new().add_file_with_mime_type("share.txt", ""),
            ShareSheet::new().add_file_uri_with_mime_type("content://robius/share.txt", ""),
        ];

        for payload in invalid_payloads {
            assert!(matches!(payload.options.validate(), Err(Error::InvalidItem)));
        }
    }

    #[test]
    fn text_and_urls_are_combined_in_order() {
        let sheet = ShareSheet::new()
            .add_text("hello")
            .add_url("https://robius.rs/")
            .add_text("goodbye");

        assert_eq!(
            shared_text(&sheet.options).as_deref(),
            Some("hello\nhttps://robius.rs/\ngoodbye"),
        );
    }

    #[test]
    fn file_items_preserve_paths_uris_and_mime_types() {
        let sheet = ShareSheet::new()
            .add_file_with_mime_type("share.txt", "text/plain")
            .add_file_uri_with_mime_type("content://robius/share.png", "image/png");

        let files = file_items(&sheet.options).collect::<Vec<_>>();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path(), Some(Path::new("share.txt")));
        assert_eq!(files[0].uri(), None);
        assert_eq!(files[0].mime_type(), Some("text/plain"));
        assert_eq!(files[1].path(), None);
        assert_eq!(files[1].uri(), Some("content://robius/share.png"));
        assert_eq!(files[1].mime_type(), Some("image/png"));
    }

    #[test]
    fn valid_builder_options_pass_validation() {
        let sheet = ShareSheet::new()
            .set_title("Share")
            .set_subject("Robius")
            .add_text("hello")
            .add_url("https://robius.rs/")
            .add_file("share.txt")
            .add_file_uri("content://robius/share.txt");

        assert!(sheet.options.validate().is_ok());
    }
}
