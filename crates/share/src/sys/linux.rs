use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{shared_text, Error, Result, ShareItem, ShareOptions, SharedFile};

static TEMP_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn share(options: ShareOptions) -> Result<()> {
    let payload = LinuxPayload::new(&options)?;
    payload.open()
}

enum LinuxPayload {
    Uri(String),
    Path(PathBuf),
}

impl LinuxPayload {
    fn new(options: &ShareOptions) -> Result<Self> {
        if options.items.len() == 1 {
            return match &options.items[0] {
                ShareItem::Text(text) => Ok(Self::Path(write_temp_text_file(text)?)),
                ShareItem::Url(url) => Ok(Self::Uri(url.clone())),
                ShareItem::File(file) => file_payload(file),
            };
        }

        Ok(Self::Path(write_temp_text_file(&manifest_text(options))?))
    }

    fn open(self) -> Result<()> {
        match self {
            LinuxPayload::Uri(uri) => open_uri(&uri),
            LinuxPayload::Path(path) => open_path(&path),
        }
    }
}

fn file_payload(file: &SharedFile) -> Result<LinuxPayload> {
    if let Some(path) = file.path() {
        let path = std::fs::canonicalize(path)?;
        return Ok(LinuxPayload::Path(path.to_owned()));
    }

    let uri = file.uri().ok_or(Error::InvalidItem)?;
    if let Some(path) = file_uri_to_path(uri) {
        std::fs::metadata(&path)?;
        return Ok(LinuxPayload::Path(path));
    }

    Ok(LinuxPayload::Uri(uri.to_owned()))
}

fn open_uri(uri: &str) -> Result<()> {
    try_portal_open_uri(uri)
        .or_else(|_| run_command("xdg-open", [OsString::from(uri)]))
        .map_err(|err| match err {
            CommandError::NoHandler => Error::NoHandler,
            CommandError::Io(err) => Error::Io(err),
        })
}

fn open_path(path: &Path) -> Result<()> {
    try_portal_open_file(path)
        .or_else(|_| run_command("xdg-open", [path.as_os_str().to_owned()]))
        .map_err(|err| match err {
            CommandError::NoHandler => Error::NoHandler,
            CommandError::Io(err) => Error::Io(err),
        })
}

fn try_portal_open_uri(uri: &str) -> std::result::Result<(), CommandError> {
    run_command(
        "gdbus",
        [
            OsString::from("call"),
            OsString::from("--session"),
            OsString::from("--dest"),
            OsString::from("org.freedesktop.portal.Desktop"),
            OsString::from("--object-path"),
            OsString::from("/org/freedesktop/portal/desktop"),
            OsString::from("--method"),
            OsString::from("org.freedesktop.portal.OpenURI.OpenURI"),
            OsString::from(gvariant_string("")),
            OsString::from(gvariant_string(uri)),
            OsString::from("{'ask': <true>}"),
        ],
    )
}

fn try_portal_open_file(path: &Path) -> std::result::Result<(), CommandError> {
    let file = File::open(path).map_err(CommandError::Io)?;
    run_command_with_stdin(
        "gdbus",
        [
            OsString::from("call"),
            OsString::from("--session"),
            OsString::from("--dest"),
            OsString::from("org.freedesktop.portal.Desktop"),
            OsString::from("--object-path"),
            OsString::from("/org/freedesktop/portal/desktop"),
            OsString::from("--method"),
            OsString::from("org.freedesktop.portal.OpenURI.OpenFile"),
            OsString::from(gvariant_string("")),
            OsString::from("0"),
            OsString::from("{'ask': <true>}"),
        ],
        file,
    )
}

fn run_command(
    program: &str,
    args: impl IntoIterator<Item = OsString>,
) -> std::result::Result<(), CommandError> {
    let status = Command::new(program).args(args).status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(command_status_error(status)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(CommandError::NoHandler),
        Err(err) => Err(CommandError::Io(err)),
    }
}

fn run_command_with_stdin(
    program: &str,
    args: impl IntoIterator<Item = OsString>,
    stdin: File,
) -> std::result::Result<(), CommandError> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::from(stdin))
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(command_status_error(status)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(CommandError::NoHandler),
        Err(err) => Err(CommandError::Io(err)),
    }
}

fn command_status_error(_status: ExitStatus) -> CommandError {
    CommandError::NoHandler
}

fn write_temp_text_file(text: &str) -> Result<PathBuf> {
    let temp_dir = temp_share_dir()?;
    for _ in 0..100 {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = temp_dir.join(format!(
            "robius-share-{}-{counter}.txt",
            std::process::id(),
        ));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(Error::Io(err)),
        }
    }

    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create a temporary share payload",
    )))
}

fn temp_share_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("robius-share");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

fn manifest_text(options: &ShareOptions) -> String {
    let mut text = String::new();

    if let Some(title) = &options.title {
        text.push_str(title);
        text.push('\n');
        text.push('\n');
    }
    if let Some(subject) = &options.subject {
        text.push_str(subject);
        text.push('\n');
        text.push('\n');
    }
    if let Some(shared_text) = shared_text(options) {
        text.push_str(&shared_text);
        text.push('\n');
        text.push('\n');
    }

    for item in &options.items {
        match item {
            ShareItem::Text(_) | ShareItem::Url(_) => {}
            ShareItem::File(file) => {
                if let Some(path) = file.path() {
                    text.push_str(&path.display().to_string());
                    text.push('\n');
                } else if let Some(uri) = file.uri() {
                    text.push_str(uri);
                    text.push('\n');
                }
            }
        }
    }

    text.trim_end().to_owned()
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let uri = uri.strip_prefix("file://")?;
    let uri = uri.split(['?', '#']).next().unwrap_or(uri);
    let uri = if uri.starts_with("localhost/") {
        &uri["localhost".len()..]
    } else if !uri.starts_with('/') {
        return None;
    } else {
        uri
    };
    percent_decode(uri).map(PathBuf::from)
}

fn percent_decode(value: &str) -> Option<OsString> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = input.next()?;
            let low = input.next()?;
            bytes.push((hex_value(high)? << 4) | hex_value(low)?);
        } else {
            bytes.push(byte);
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(OsString::from_vec(bytes))
    }
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn gvariant_string(value: &str) -> String {
    let mut escaped = String::from("'");
    for ch in value.chars() {
        if ch == '\'' || ch == '\\' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped.push('\'');
    escaped
}

enum CommandError {
    NoHandler,
    Io(std::io::Error),
}
