use robius_proxy::{ProxyManager, ProxyMode, ProxyState};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = ProxyManager::new()?;
    let state = manager.current()?;

    println!("Current proxy configuration:");
    print_state(&state);

    Ok(())
}

fn print_state(state: &ProxyState) {
    match &state.mode {
        ProxyMode::Direct => println!("  mode: Direct (no proxy)"),
        ProxyMode::Manual(settings) => {
            println!("  mode: Manual");
            if let Some(http) = &settings.http {
                println!("    http: {}:{}", http.host, http.port);
            }
            if let Some(https) = &settings.https {
                println!("    https: {}:{}", https.host, https.port);
            }
            if let Some(socks) = &settings.socks {
                println!("    socks: {}:{}", socks.host, socks.port);
            }
            if settings.bypass.entries.is_empty() {
                println!("    bypass: (none)");
            } else {
                println!("    bypass:");
                for entry in &settings.bypass.entries {
                    println!("      - {entry}");
                }
            }
        }
        ProxyMode::AutoConfigUrl { url, bypass } => {
            println!("  mode: PAC url");
            println!("    url: {url}");
            if bypass.entries.is_empty() {
                println!("    bypass: (none)");
            } else {
                println!("    bypass:");
                for entry in &bypass.entries {
                    println!("      - {entry}");
                }
            }
        }
        ProxyMode::AutoDiscovery { bypass } => {
            println!("  mode: Auto-discovery (WPAD)");
            if bypass.entries.is_empty() {
                println!("    bypass: (none)");
            } else {
                println!("    bypass:");
                for entry in &bypass.entries {
                    println!("      - {entry}");
                }
            }
        }
    }
}
