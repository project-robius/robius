# robius-web-auth-session

Rust abstractions for OS-provided web auth sessions, which are typically used for OAuth/SSO login procedures. Using this will cause the OS to present an in-app mini browser window (or rather, in front of the app while your app is still in the foreground), and then it can route the auth redirect back to your app with the custom URL scheme of your choosing.
scheme.

## Use cases, or why this exists

The standard pattern of starting a local HTTP server on `localhost:whatever_port` and using that as the OAuth/SSO URL does work well on desktop, but not on iOS. That's because iOS will bring the browser to the foreground, putting your app in the background, which will suspend your app's threads/socket connections. This causes your app to stop listening for responses on the SSO URL, which means that the login will just hang infinitely.

## Platform behavior

Currently this crate is iOS-only. Other platforms return `Error::Unsupported`.

On iOS, this uses `ASWebAuthenticationSession`, which is the recommended way of doing in-app SSO-like logins since iOS 12 or so.

On Android, we could do something similar via custom chrom tabs with a scheme intent (like `yourapp://login/...`).

This isn't really needed on desktop platforms that permit regular multitasking, meaning you can log in on your browser and then go back to the app without issues.


## Example

```rust,ignore
use robius_web_auth_session::AuthSession;

AuthSession::new(
    "https://example.com/oauth/authorize?...&redirect_uri=myapp%3A%2F%2Fcallback",
    "myapp",
)
.start(|result| {
    match result {
        Ok(url) => {
            // This `url` is something like "myapp://callback?code=..."
            // You can parse the OAuth/SSO code from the query URL (the part after `?`)
        }
        Err(err) => {
            // The user cancelled the login, or there was another error.
            eprintln!("Auth session failed: {err:?}");
        }
    }
})
.expect("must be called from the main UI thread");
```

## AI usage
Claude Code was used as an assistant in the implementation of this crate, but with heavy manual editing of the code and various changes/fixes to ensure quality.
