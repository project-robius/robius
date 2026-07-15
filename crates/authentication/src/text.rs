/// The text contents displayed by an authentication prompt.
pub struct Text<'a, 'b, 'c, 'd, 'e, 'f> {
    /// The text of the authentication prompt on Android.
    pub android: AndroidText<'a, 'b, 'c>,
    /// The description of the authentication prompt on Apple devices.
    ///
    /// Appears as "$(binary_name) is trying to $(description)".
    pub apple: &'d str,
    /// The description of the authentication prompt on Windows.
    pub windows: WindowsText<'e, 'f>,
}

/// The text of the authentication prompt on Android.
pub struct AndroidText<'a, 'b, 'c> {
    pub title: &'a str,
    pub subtitle: Option<&'b str>,
    pub description: Option<&'c str>,
}

/// The text of the authentication prompt on Windows,
/// including a title ("caption") and description ("message").
pub struct WindowsText<'a, 'b> {
    #[allow(dead_code)]
    pub(crate) title: &'a str,
    #[allow(dead_code)]
    pub(crate) description: &'b str,
}

impl<'a, 'b> WindowsText<'a, 'b> {
    /// Creates a new `WindowsText` instance.
    ///
    /// Returns `None` if `title` exceeds 128 bytes in length
    /// or if `description` exceeds 1024 bytes in length.
    #[cfg(target_os = "windows")]
    pub const fn new(title: &'a str, description: &'b str) -> Option<Self> {
        use windows::Win32::Security::Credentials::{
            CREDUI_MAX_CAPTION_LENGTH, CREDUI_MAX_MESSAGE_LENGTH,
        };

        if title.len() <= CREDUI_MAX_CAPTION_LENGTH as usize
            && description.len() <= CREDUI_MAX_MESSAGE_LENGTH as usize
        {
            Some(Self { title, description })
        } else {
            None
        }
    }

    /// Creates a new `WindowsText` instance.
    ///
    /// On Windows, returns `None` if `title` exceeds 128 bytes in length
    /// or if `description` exceeds 1024 bytes in length. On other targets the
    /// text is unused, so no validation is performed and this always returns
    /// `Some`.
    #[cfg(not(target_os = "windows"))]
    pub const fn new(title: &'a str, description: &'b str) -> Option<Self> {
        Some(Self { title, description })
    }

    /// Creates a new `WindowsText` instance.
    ///
    /// The `title` ("caption") will be truncated to at most 128 bytes in length,
    /// and the `description` ("message") will be truncated to at most 1024 bytes in length.
    /// Truncation respects UTF-8 character boundaries, so fewer bytes may be kept.
    #[cfg(target_os = "windows")]
    pub fn new_truncated(title: &'a str, description: &'b str) -> Self {
        use windows::Win32::Security::Credentials::{
            CREDUI_MAX_CAPTION_LENGTH, CREDUI_MAX_MESSAGE_LENGTH,
        };

        Self {
            title: truncate_to_char_boundary(title, CREDUI_MAX_CAPTION_LENGTH as usize),
            description: truncate_to_char_boundary(
                description,
                CREDUI_MAX_MESSAGE_LENGTH as usize,
            ),
        }
    }

    /// Creates a new `WindowsText` instance.
    ///
    /// On Windows, the `title` ("caption") is truncated to at most 128 bytes and
    /// the `description` ("message") to at most 1024 bytes, respecting UTF-8
    /// character boundaries. On other targets the text is unused and is stored
    /// verbatim without truncation.
    #[cfg(not(target_os = "windows"))]
    pub fn new_truncated(title: &'a str, description: &'b str) -> Self {
        Self { title, description }
    }
}

/// Truncates `s` to at most `max_len` bytes without splitting a UTF-8 character.
#[cfg(target_os = "windows")]
fn truncate_to_char_boundary(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
