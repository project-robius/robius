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

### Add a polkit action policy file


#### Development & debugging

During development and debugging, you can simplify the process using the following steps:

1. Place the policy file within the project, for example in resources/com.yourapp.policy.

2. Install it once on your development machine using sudo:

```bash
sudo install -Dm644 resources/com.yourapp.policy \
  /usr/share/polkit-1/actions/com.yourapp.policy
```

3. Verify functionality with pkaction:

```bash
pkaction --action-id com.yourapp.authenticate --verbose
```

Log back into your desktop session (or restart polkitd), then run your example.


Policy file example:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<policyconfig>
  <action id="com.yourapp.authenticate">
    <description>Authenticate to use YourApp</description>
    <message>Authentication is required</message>
    <defaults>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
  </action>
</policyconfig>
````
#### 

> [!TIP]
> Automatic addition during runtime is not recommended and should generally be avoided.
The reason is not technical feasibility, but rather security/distribution policy restrictions:
> 
> polkit actions (.policy files) are system-level security policies. By design, they must be written by package managers or administrators during installation. Ordinary applications should not be allowed to modify system security configurations during runtime.
> 
> Writing to `/usr/share/...` during runtime requires root privileges; allowing an app to elevate privileges to modify policy files is flagged as a security red flag by many distributions.
>
> The only recommended “automatic method” is installation-time automation.
This means packaging the `.policy` file alongside your deb/rpm/aur/flatpak package during installation.

## Example

```rust
use robius_authentication::{
    AndroidText, BiometricStrength, Context, Policy, PolicyBuilder, Text, WindowsText,
};

// Linux ignores policy options like biometrics/password (kept for parity).
let policy: Policy = PolicyBuilder::new()
    // The action ID must match your `.policy`.
    .action_id("com.yourapp.authenticate") // optional if using default
    .biometrics(Some(BiometricStrength::Strong))
    .password(true)
    .companion(true)
    .build()
    .unwrap();

let text = Text {
    android: AndroidText {
        title: "Title",
        subtitle: None,
        description: None,
    },
    apple: "authenticate",
    windows: WindowsText::new("Title", "Description"),
};

let callback = |auth_result| {
    match auth_result {
        Ok(_)  => log::info!("Authentication success!"),
        Err(_) => log::error!(Authentication failed!"),
    }
};


Context::new(())
    .authenticate(text, &policy, callback)
    .expect("Authentication failed");
```

For more details about the prompt text, see the `Text` struct,
which allows you to customize the prompt for each platform.

[`polkit`]: https://www.freedesktop.org/software/polkit/docs/latest/polkit.8.html
