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
    /// Returns `None` if `title` exceeds 128 bytes in length
    /// or if `description` exceeds 1024 bytes in length.
    #[cfg(not(target_os = "windows"))]
    pub const fn new(title: &'a str, description: &'b str) -> Option<Self> {
        Some(Self { title, description })
    }

    /// Creates a new `WindowsText` instance.
    ///
    /// The `title` ("caption") will be truncated to 128 bytes in length,
    /// and the `description` ("message") will be truncated to 1024 bytes in length.
    #[cfg(target_os = "windows")]
    pub fn new_truncated(title: &'a str, description: &'b str) -> Self {
        use windows::Win32::Security::Credentials::{
            CREDUI_MAX_CAPTION_LENGTH, CREDUI_MAX_MESSAGE_LENGTH,
        };

        let title_max_len = std::cmp::min(CREDUI_MAX_CAPTION_LENGTH as usize, title.len());
        let description_max_len = std::cmp::min(CREDUI_MAX_MESSAGE_LENGTH as usize, description.len());
        Self {
            title: &title[..title_max_len],
            description: &description[..description_max_len],
        }
    }

    /// Creates a new `WindowsText` instance.
    ///
    /// The `title` ("caption") will be truncated to 128 bytes in length,
    /// and the `description` ("message") will be truncated to 1024 bytes in length.
    #[cfg(not(target_os = "windows"))]
    pub fn new_truncated(title: &'a str, description: &'b str) -> Self {
        Self { title, description }
    }
}
