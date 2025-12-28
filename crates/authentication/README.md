# `robius-authentication`

[![Latest Version](https://img.shields.io/crates/v/robius-authentication.svg)](https://crates.io/crates/robius_authentication)
[![Docs](https://docs.rs/robius-authentication/badge.svg)](https://docs.rs/robius-authentication/latest/robius_authentication/)
[![Project Robius Matrix Chat](https://img.shields.io/matrix/robius-general%3Amatrix.org?server_fqdn=matrix.org&style=flat&logo=matrix&label=Project%20Robius%20Matrix%20Chat&color=B7410E)](https://matrix.to/#/#robius:matrix.org)

Rust abstractions for multi-platform native authentication.

This crate supports:
* Apple: TouchID, FaceID, and regular username/password on both macOS and iOS.
  * Requires the `NSFaceIDUsageDescription` key in your app's `Info.plist` file.
* Android: Biometric prompt and regular screen lock. See below for additional steps.
  * Requires the `USE_BIOMETRIC` permission in your app's manifest.
* Windows: Windows Hello (face recognition, fingerprint, PIN),
plus winrt-based fallback for username/password.
* Linux: [`polkit`]-based authentication using the desktop environment's prompt.
  * **Note: Linux support is currently incomplete.**


## Usage on iOS
To use this crate on iOS, you must add the following to your app's `Info.plist`:
```xml
<key>NSFaceIDUsageDescription</key>
<string>Insert your usage description here</string>
```

## Usage on Android
To use this crate on Android, you must add the following to your app's `AndroidManifest.xml`:
```xml
<uses-permission android:name="android.permission.USE_BIOMETRIC" />
```

## Usage on Linux

On Linux, `robius-authentication` uses **polkit** to request authorization via the
desktop environment's native authentication prompt (GNOME/KDE/etc).

> [!IMPORTANT]
> **Ensure a polkit agent is running**
>
> The prompt is displayed by a polkit authentication agent (GNOME/KDE usually start one automatically).
> If no agent is running (headless/SSH), no prompt will appear and auth will fail.

### Write policy file.

You can crate your own application's policy file, also can crate by template policy file.

Template policy file see: [`./examples/org.robius.authentication.policy`](./examples/org.robius.authentication.policy)

### Quick Test Mode ⚠️

Add the policy file to actions by manually executing the following command:

```bash
sudo install -Dm644 com.yourapp.policy /usr/share/polkit-1/actions/
```

Then, ensure your policy file is correctly installed.

```bash
pkaction --action-id <YOUR_POLICY_File_ACTION_ID>
```

> During the test mode, you don't need to worry about the location of the policy file; just ensure it installs correctly.
>
> You can also store it in advance under the unified packaging configuration folder (like `packaging`)to facilitate automatic installation of the policy file during release mode when users perform installation.


### Release Mode

> The official polkit documentation explicitly states: Mechanisms should install action XML files to [/usr/share/polkit-1/actions](https://www.freedesktop.org/software/polkit/docs/latest/polkit.8.html).


As long as your packaging tool provides the capability to automatically install *.policy files under /usr/share/polkit-1/actions/.

See the example below for use `cargo-packager`.


#### Use `cargo-packager`

```toml
# https://docs.crabnebula.dev/packager/configuration/#debianconfig
[package.metadata.packager.deb]
depends = "./dist/depends_deb.txt"
desktop_template = "./packaging/robrix.desktop"
section = "utils"

[package.metadata.packager.deb.files]
"./packaging/org.robius.authentication.policy" = "/usr/share/polkit-1/actions/org.robius.authentication.policy"
```

When you are packaging, `cargo-packager` automatically installs files to their target direactory.