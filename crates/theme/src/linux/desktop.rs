//! The theme that a desktop's own settings name. Desktops older than the portal's
//! `color-scheme` only name a GTK theme (or, on KDE, a window color), and window
//! managers without a desktop keep it in GTK's `settings.ini`.
//!
//! Their settings files are followed with inotify, so a change reaches subscribers
//! even where the portal doesn't report it.

use std::{
    env,
    ffi::{c_char, c_int, CString},
    fs::{self, File},
    io::Read,
    os::{fd::FromRawFd, unix::ffi::OsStrExt},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::Theme;

/// Where Xfce's settings daemon keeps the GTK theme's name, in `xsettings.xml`.
const XFCE_SETTINGS_DIR: &str = "xfce4/xfconf/xfce-perchannel-xml";
/// The files whose changes can change the theme: KDE's, dconf's (GNOME, Cinnamon, MATE), GTK's and Xfce's.
const SETTINGS_FILES: [&[u8]; 4] = [b"kdeglobals", b"user", b"settings.ini", b"xsettings.xml"];

const IN_CLOEXEC: c_int = 0o2000000;
const IN_CLOSE_WRITE: u32 = 0x08;
const IN_MOVED_TO: u32 = 0x80;
const IN_DELETE: u32 = 0x200;

extern "C" {
    fn inotify_init1(flags: c_int) -> c_int;
    fn inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> c_int;
}

/// The desktops whose settings are read differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Desktop {
    Gnome,
    Kde,
    Xfce,
    Cinnamon,
    Mate,
    Deepin,
    Other,
}

impl Desktop {
    /// The desktop that `XDG_CURRENT_DESKTOP` names.
    pub(super) fn current() -> Self {
        let names = env::var("XDG_CURRENT_DESKTOP").or_else(|_| env::var("DESKTOP_SESSION")).unwrap_or_default();
        Self::named(&names)
    }

    /// The first known desktop in a list like `ubuntu:GNOME`.
    fn named(names: &str) -> Self {
        let names = names.to_ascii_lowercase();
        for name in names.split(':') {
            match name {
                "kde" | "plasma" => return Self::Kde,
                "xfce" | "xubuntu" => return Self::Xfce,
                "x-cinnamon" | "cinnamon" => return Self::Cinnamon,
                "mate" => return Self::Mate,
                "deepin" | "dde" => return Self::Deepin,
                "gnome" | "gnome-classic" | "gnome-flashback" | "ubuntu" | "unity" | "budgie" | "pantheon" | "pop" => {
                    return Self::Gnome
                }
                _ => {}
            }
        }
        Self::Other
    }

    /// Whether a `color-scheme` of "no preference" is this desktop's own answer, rather than a default
    /// that it never changes. GNOME 42+ keeps it current, and KDE always says light or dark.
    pub(super) fn is_color_scheme_kept(self) -> bool {
        matches!(self, Self::Gnome | Self::Kde)
    }

    /// The portal setting, besides `color-scheme`, that this desktop keeps its theme in.
    pub(super) fn portal_setting(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Gnome => Some(("org.gnome.desktop.interface", "gtk-theme")),
            Self::Kde => Some(("org.kde.kdeglobals.Colors:Window", "BackgroundNormal")),
            _ => None,
        }
    }

    /// The theme that this desktop's own settings name, read without the portal.
    pub(super) fn theme(self) -> Option<Theme> {
        let setting = match self {
            Self::Gnome => gsettings("org.gnome.desktop.interface"),
            Self::Cinnamon => gsettings("org.cinnamon.desktop.interface"),
            Self::Mate => gsettings("org.mate.interface"),
            Self::Deepin => gsettings("com.deepin.dde.appearance"),
            Self::Xfce => xfce_theme_name(),
            Self::Kde => kde_window_color(),
            Self::Other => None,
        };
        setting.as_deref().and_then(theme_of).or_else(gtk_settings_theme)
    }
}

/// The theme that a GTK or KDE theme's name, or KDE's `r,g,b` window color, stands for.
pub(super) fn theme_of(text: &str) -> Option<Theme> {
    let text = text.trim().trim_matches(|c| c == '\'' || c == '"');
    if let Some(gray) = gray(text) {
        // The same test as KDE's own portal.
        return Some(if gray < 192 { Theme::Dark } else { Theme::Light });
    }
    if text.is_empty() {
        return None;
    }
    let name = text.to_ascii_lowercase();
    // A "darker" theme (like Arc-Darker) only has dark title bars.
    let is_dark = (name.contains("dark") && !name.contains("darker")) || DARK_THEMES.iter().any(|dark| name.contains(dark));
    Some(if is_dark { Theme::Dark } else { Theme::Light })
}

/// Popular dark themes whose names don't say "dark".
const DARK_THEMES: [&str; 6] = ["highcontrastinverse", "dracula", "nordic", "nokto", "noir", "equilux"];

/// The gray level of an `r,g,b` color, as Qt's `qGray` computes it.
fn gray(text: &str) -> Option<u32> {
    let mut channels = text.split(',').map(|channel| channel.trim().parse::<u8>().ok().map(u32::from));
    let (red, green, blue) = (channels.next()??, channels.next()??, channels.next()??);
    Some((red * 11 + green * 16 + blue * 5) / 32)
}

/// The `gtk-theme` of a GSettings schema, as the `gsettings` tool prints it.
fn gsettings(schema: &str) -> Option<String> {
    let mut child = Command::new("gsettings")
        .args(["get", schema, "gtk-theme"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // It answers in a few milliseconds, but a stuck one must not hold up the theme.
    let deadline = Instant::now() + Duration::from_secs(1);
    while child.try_wait().ok()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Xfce's GTK theme name, from the file that its settings daemon keeps it in.
fn xfce_theme_name() -> Option<String> {
    let xml = fs::read_to_string(config_home()?.join(XFCE_SETTINGS_DIR).join("xsettings.xml")).ok()?;
    let property = &xml[xml.find(r#"name="ThemeName""#)?..];
    let value = &property[property.find(r#"value=""#)? + r#"value=""#.len()..];
    Some(value[..value.find('"')?].to_owned())
}

/// KDE's window background color, from its `kdeglobals` file.
fn kde_window_color() -> Option<String> {
    let kdeglobals = fs::read_to_string(config_home()?.join("kdeglobals")).ok()?;
    ini_value(&kdeglobals, "Colors:Window", "BackgroundNormal").map(str::to_owned)
}

/// The theme in GTK's `settings.ini` files: the user's, then the system's.
fn gtk_settings_theme() -> Option<Theme> {
    let system_dirs = env::var("XDG_CONFIG_DIRS").ok().filter(|dirs| !dirs.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    let system_dirs = env::split_paths(&system_dirs).chain([PathBuf::from("/etc")]).collect::<Vec<_>>();
    for dir in config_home().into_iter().chain(system_dirs) {
        for gtk in ["gtk-3.0", "gtk-4.0"] {
            let Ok(ini) = fs::read_to_string(dir.join(gtk).join("settings.ini")) else {
                continue;
            };
            let prefer_dark = ini_value(&ini, "Settings", "gtk-application-prefer-dark-theme");
            if prefer_dark.is_some_and(|value| matches!(value, "1" | "true")) {
                return Some(Theme::Dark);
            }
            if let Some(theme) = ini_value(&ini, "Settings", "gtk-theme-name").and_then(theme_of) {
                return Some(theme);
            }
        }
    }
    None
}

/// The user's configuration directory.
fn config_home() -> Option<PathBuf> {
    let config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|dir| dir.is_absolute());
    config_home.or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))
}

/// The last value of `key` in `[section]` of an INI file.
fn ini_value<'a>(ini: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut is_in_section = false;
    let mut found = None;
    for line in ini.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) {
            is_in_section = name == section;
        } else if let Some((name, value)) = line.split_once('=').filter(|_| is_in_section) {
            if name.trim() == key {
                found = Some(value.trim());
            }
        }
    }
    found
}

/// Whether `follow_files` has started.
static IS_FOLLOWING: AtomicBool = AtomicBool::new(false);

pub(super) fn is_following_files() -> bool {
    IS_FOLLOWING.load(Ordering::Acquire)
}

/// Calls `on_change` from a thread of its own each time one of the settings files above changes.
/// It does this once per process; a directory that doesn't exist yet isn't followed.
pub(super) fn follow_files(on_change: fn()) {
    if IS_FOLLOWING.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(config) = config_home() else { return };
    // SAFETY: this only makes a new descriptor.
    let fd = unsafe { inotify_init1(IN_CLOEXEC) };
    if fd < 0 {
        return;
    }
    // SAFETY: `fd` is new, and this `File` is its only owner.
    let mut events = unsafe { File::from_raw_fd(fd) };
    for dir in [config.clone(), config.join("dconf"), config.join("gtk-3.0"), config.join("gtk-4.0"), config.join(XFCE_SETTINGS_DIR)] {
        let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else { continue };
        // SAFETY: `path` is NUL-terminated and outlives the call. A missing directory just fails.
        unsafe { inotify_add_watch(fd, path.as_ptr(), IN_CLOSE_WRITE | IN_MOVED_TO | IN_DELETE) };
    }
    let follow = move || {
        let mut buffer = [0u8; 4096];
        while let Ok(length) = events.read(&mut buffer) {
            if file_names(&buffer[..length]).any(|name| SETTINGS_FILES.contains(&name)) {
                on_change();
            }
        }
    };
    let _ = thread::Builder::new().name("robius-theme-files".into()).spawn(follow);
}

/// The file names in a buffer of inotify events.
fn file_names(mut events: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        // Each event is `wd`, `mask`, `cookie` and `len`, then a name padded with NULs to `len` bytes.
        let length = u32::from_ne_bytes(events.get(12..16)?.try_into().ok()?) as usize;
        let name = events.get(16..16 + length)?;
        events = &events[16 + length..];
        Some(name.split(|&byte| byte == 0).next().unwrap_or_default())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktops_are_known_by_any_of_their_names() {
        assert_eq!(Desktop::named("ubuntu:GNOME"), Desktop::Gnome);
        assert_eq!(Desktop::named("Budgie:GNOME"), Desktop::Gnome);
        assert_eq!(Desktop::named("KDE"), Desktop::Kde);
        assert_eq!(Desktop::named("X-Cinnamon"), Desktop::Cinnamon);
        assert_eq!(Desktop::named("MATE"), Desktop::Mate);
        assert_eq!(Desktop::named("XFCE"), Desktop::Xfce);
        assert_eq!(Desktop::named("Deepin"), Desktop::Deepin);
        assert_eq!(Desktop::named("i3"), Desktop::Other);
        assert_eq!(Desktop::named(""), Desktop::Other);
    }

    #[test]
    fn theme_names_and_window_colors() {
        assert_eq!(theme_of("'Yaru-dark'"), Some(Theme::Dark));
        assert_eq!(theme_of("Mint-Y-Dark-Aqua"), Some(Theme::Dark));
        assert_eq!(theme_of("BreezeDark"), Some(Theme::Dark));
        assert_eq!(theme_of("Yaru"), Some(Theme::Light));
        assert_eq!(theme_of("Arc-Darker"), Some(Theme::Light));
        assert_eq!(theme_of("HighContrastInverse"), Some(Theme::Dark));
        assert_eq!(theme_of("Dracula"), Some(Theme::Dark));
        assert_eq!(theme_of("Adapta-Nokto-Eta"), Some(Theme::Dark));
        assert_eq!(theme_of("239,240,241"), Some(Theme::Light));
        assert_eq!(theme_of("32, 35, 38"), Some(Theme::Dark));
        assert_eq!(theme_of(" "), None);
    }

    #[test]
    fn ini_values_come_from_their_own_section() {
        let ini = "[Other]\ngtk-theme-name=Wrong\n[Settings]\n# gtk-theme-name=Commented\ngtk-theme-name = Adwaita-dark\n";
        assert_eq!(ini_value(ini, "Settings", "gtk-theme-name"), Some("Adwaita-dark"));
        assert_eq!(ini_value(ini, "Settings", "gtk-application-prefer-dark-theme"), None);
    }

    #[test]
    fn file_names_are_read_from_inotify_events() {
        let mut events = Vec::new();
        for name in [&b"user\0\0\0\0"[..], &b"settings.ini\0\0\0\0"[..]] {
            events.extend_from_slice(&[0; 12]);
            events.extend_from_slice(&(name.len() as u32).to_ne_bytes());
            events.extend_from_slice(name);
        }
        assert_eq!(file_names(&events).collect::<Vec<_>>(), [&b"user"[..], &b"settings.ini"[..]]);
    }
}
