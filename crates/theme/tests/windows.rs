//! Follows a real change of the Windows app theme from end to end.
#![cfg(windows)]

use std::{process::Command, sync::mpsc, time::Duration};

use robius_theme::Theme;

const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const VALUE: &str = "AppsUseLightTheme";
/// How long a change may take to reach the subscriber.
const TIMEOUT: Duration = Duration::from_secs(5);

/// This changes the current user's app theme setting for a moment, so it only
/// runs when asked for, as CI does.
#[test]
#[ignore = "changes the current user's app theme setting for a moment"]
fn a_change_of_the_app_theme_reaches_a_subscriber() {
    // `reg` prints the stored number after its type, and fails if nothing is stored.
    let original = reg("query", &[]).map(|printed| {
        let mut words = printed.split_whitespace().skip_while(|word| *word != "REG_DWORD");
        words.nth(1).expect("the setting should be stored as a number").to_owned()
    });

    let (sender, themes) = mpsc::channel();
    let _subscription = robius_theme::subscribe(move |theme| {
        let _ = sender.send(theme);
    })
    .unwrap();
    let before = themes.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(robius_theme::query(), Ok(before));
    let (opposite, number) = if before == Some(Theme::Dark) { (Theme::Light, "1") } else { (Theme::Dark, "0") };

    let restore = Restore(original);
    assert!(reg("add", &["/t", "REG_DWORD", "/d", number, "/f"]).is_some());
    assert_eq!(themes.recv_timeout(TIMEOUT), Ok(Some(opposite)));
    assert_eq!(robius_theme::query(), Ok(Some(opposite)));

    drop(restore);
    assert_eq!(themes.recv_timeout(TIMEOUT), Ok(before));
    assert_eq!(robius_theme::query(), Ok(before));
}

/// Puts the setting back when it's dropped, which also happens when the test fails.
struct Restore(Option<String>);

impl Drop for Restore {
    fn drop(&mut self) {
        match &self.0 {
            Some(number) => reg("add", &["/t", "REG_DWORD", "/d", number, "/f"]),
            None => reg("delete", &["/f"]),
        };
    }
}

/// Runs the `reg` tool on the setting, and returns what it printed if it succeeded.
fn reg(action: &str, more: &[&str]) -> Option<String> {
    let output = Command::new("reg").args([action, KEY, "/v", VALUE]).args(more).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
