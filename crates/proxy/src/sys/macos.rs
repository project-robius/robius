use std::{
    collections::HashMap,
    ffi::OsStr,
    process::{Command, Stdio},
};

use crate::{
    BypassList, Error, ProxyEndpoint, ProxyMode, ProxySettings, ProxyState, Result,
};

pub(crate) struct Manager {
    // Network service names returned by `networksetup -listallnetworkservices`.
    services: Vec<String>,
}

impl Manager {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            services: list_services()?,
        })
    }

    pub(crate) fn current(&self) -> Result<ProxyState> {
        let output = Command::new("scutil")
            .arg("--proxy")
            .output()
            .map_err(Error::Io)?;

        if !output.status.success() {
            return Err(Error::CommandFailed {
                command: "scutil --proxy".to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_scutil_proxy(&stdout)
    }

    pub(crate) fn apply(&self, state: ProxyState) -> Result<()> {
        match state.mode {
            ProxyMode::Direct => self.disable_all(),
            ProxyMode::Manual(settings) => self.apply_manual(settings),
            ProxyMode::AutoConfigUrl { url, bypass } => self.apply_pac(url, bypass),
            ProxyMode::AutoDiscovery { bypass } => self.apply_auto_discovery(bypass),
        }
    }

    fn apply_manual(&self, mut settings: ProxySettings) -> Result<()> {
        // Drop accidental empty entries so we do not feed blank values to `networksetup`.
        settings.bypass.entries.retain(|s| !s.is_empty());
        for service in &self.services {
            self.disable_auto(service)?;

            configure_endpoint(
                service,
                "-setwebproxy",
                "-setwebproxystate",
                settings.http.as_ref(),
            )?;
            configure_endpoint(
                service,
                "-setsecurewebproxy",
                "-setsecurewebproxystate",
                settings.https.as_ref(),
            )?;
            configure_endpoint(
                service,
                "-setsocksfirewallproxy",
                "-setsocksfirewallproxystate",
                settings.socks.as_ref(),
            )?;

            self.set_bypass(service, &settings.bypass)?;
        }
        Ok(())
    }

    fn apply_pac(&self, url: String, bypass: BypassList) -> Result<()> {
        if url.is_empty() {
            return Err(Error::InvalidInput("PAC url cannot be empty"));
        }

        for service in &self.services {
            // PAC mode and manual proxies are mutually exclusive on macOS.
            self.disable_manual(service)?;
            run_networksetup([
                "-setautoproxyurl",
                service.as_str(),
                url.as_str(),
            ])?;
            run_networksetup(["-setautoproxystate", service.as_str(), "on"])?;
            self.set_bypass(service, &bypass)?;
        }
        Ok(())
    }

    fn apply_auto_discovery(&self, bypass: BypassList) -> Result<()> {
        for service in &self.services {
            self.disable_manual(service)?;
            run_networksetup(["-setproxyautodiscovery", service.as_str(), "on"])?;
            run_networksetup(["-setautoproxystate", service.as_str(), "off"])?;
            self.set_bypass(service, &bypass)?;
        }
        Ok(())
    }

    fn disable_all(&self) -> Result<()> {
        for service in &self.services {
            self.disable_manual(service)?;
            self.disable_auto(service)?;
            run_networksetup(["-setproxybypassdomains", service.as_str(), "Empty"])?;
        }
        Ok(())
    }

    fn disable_manual(&self, service: &str) -> Result<()> {
        run_networksetup(["-setwebproxystate", service, "off"])?;
        run_networksetup(["-setsecurewebproxystate", service, "off"])?;
        run_networksetup(["-setsocksfirewallproxystate", service, "off"])?;
        Ok(())
    }

    fn disable_auto(&self, service: &str) -> Result<()> {
        run_networksetup(["-setautoproxystate", service, "off"])?;
        run_networksetup(["-setproxyautodiscovery", service, "off"])?;
        Ok(())
    }

    fn set_bypass(&self, service: &str, bypass: &BypassList) -> Result<()> {
        if bypass.entries.is_empty() {
            // "Empty" is a sentinel understood by `networksetup` to clear bypass entries.
            return run_networksetup(["-setproxybypassdomains", service, "Empty"]);
        }

        let mut args = Vec::with_capacity(2 + bypass.entries.len());
        args.push("-setproxybypassdomains".to_string());
        args.push(service.to_string());
        args.extend(bypass.entries.iter().cloned());
        run_networksetup(args)
    }
}

fn list_services() -> Result<Vec<String>> {
    let output = Command::new("networksetup")
        .arg("-listallnetworkservices")
        .stderr(Stdio::null())
        .output()
        .map_err(Error::Io)?;

    if !output.status.success() {
        return Err(Error::CommandFailed {
            command: "networksetup -listallnetworkservices".to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let services: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with("An asterisk") || line.starts_with('*') {
                None
            } else {
                Some(line.to_string())
            }
        })
        .collect();

    if services.is_empty() {
        return Err(Error::Parse(
            "networksetup returned no network services".to_string(),
        ));
    }

    Ok(services)
}

fn parse_scutil_proxy(text: &str) -> Result<ProxyState> {
    let mut entries = HashMap::new();
    let mut bypass = Vec::new();
    let mut in_bypass = false;

    // The `scutil --proxy` output is a loose dictionary; walk it line by line.
    for line in text.lines() {
        let trimmed = line.trim();

        if in_bypass {
            if trimmed.starts_with('}') {
                in_bypass = false;
                continue;
            }
            if let Some((_, value)) = trimmed.split_once(':') {
                let val = value.trim().trim_matches('"').to_string();
                if !val.is_empty() {
                    bypass.push(val);
                }
            }
            continue;
        }

        if trimmed.starts_with("ExceptionsList") {
            in_bypass = true;
            continue;
        }

        if let Some((key, value)) = trimmed.split_once(" : ") {
            entries.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    let bypass = BypassList { entries: bypass };

    if entries.get("ProxyAutoConfigEnable").map(|v| v == "1").unwrap_or(false) {
        let url = entries
            .get("ProxyAutoConfigURLString")
            .map(|s| s.to_string())
            .unwrap_or_default();
        if url.is_empty() {
            return Err(Error::Parse(
                "ProxyAutoConfigEnable set but URL missing".to_string(),
            ));
        }
        return Ok(ProxyState::pac(url, bypass));
    }

    if entries
        .get("ProxyAutoDiscoveryEnable")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return Ok(ProxyState::auto_discovery(bypass));
    }

    let mut settings = ProxySettings::default();

    if entries.get("HTTPEnable").map(|v| v == "1").unwrap_or(false) {
        if let Some(endpoint) = build_endpoint(entries.get("HTTPProxy"), entries.get("HTTPPort")) {
            settings.http = Some(endpoint);
        }
    }

    if entries.get("HTTPSEnable").map(|v| v == "1").unwrap_or(false) {
        if let Some(endpoint) =
            build_endpoint(entries.get("HTTPSProxy"), entries.get("HTTPSPort"))
        {
            settings.https = Some(endpoint);
        }
    }

    if entries.get("SOCKSEnable").map(|v| v == "1").unwrap_or(false) {
        if let Some(endpoint) = build_endpoint(entries.get("SOCKSProxy"), entries.get("SOCKSPort"))
        {
            settings.socks = Some(endpoint);
        }
    }

    if settings.http.is_some() || settings.https.is_some() || settings.socks.is_some() {
        settings.bypass = bypass;
        return Ok(ProxyState::manual(settings));
    }

    Ok(ProxyState::direct())
}

fn build_endpoint(host: Option<&String>, port: Option<&String>) -> Option<ProxyEndpoint> {
    let host = host?.trim();
    let port = port?.trim();

    if host.is_empty() {
        return None;
    }
    let port: u16 = port.parse().ok()?;

    Some(ProxyEndpoint {
        host: host.to_string(),
        port,
        credentials: None,
    })
}

fn configure_endpoint(
    service: &str,
    set_cmd: &str,
    toggle_cmd: &str,
    endpoint: Option<&ProxyEndpoint>,
) -> Result<()> {
    match endpoint {
        Some(ep) => {
            let mut args = Vec::new();
            args.push(set_cmd.to_string());
            args.push(service.to_string());
            args.push(ep.host.clone());
            args.push(ep.port.to_string());

            if let Some(credentials) = &ep.credentials {
                args.push("authenticated".to_string());
                args.push(credentials.username.clone());
                if let Some(password) = &credentials.password {
                    args.push(password.clone());
                }
            }

            run_networksetup(args)?;
            run_networksetup([toggle_cmd, service, "on"])
        }
        None => run_networksetup([toggle_cmd, service, "off"]),
    }
}

fn run_networksetup<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("networksetup")
        .args(args)
        .output()
        .map_err(Error::Io)?;

    if output.status.success() {
        return Ok(());
    }

    Err(Error::CommandFailed {
        command: "networksetup".to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manual_settings() {
        let text = r#"
            <dictionary> {
                HTTPEnable : 1
                HTTPPort : 7890
                HTTPProxy : proxy.local
                HTTPSEnable : 1
                HTTPSPort : 7890
                HTTPSProxy : secure.local
                SOCKSEnable : 1
                SOCKSPort : 7890
                SOCKSProxy : socks.local
                ExceptionsList : {
                    0 : localhost
                    1 : 127.0.0.1
                }
            }
            "#;

        let state = parse_scutil_proxy(text).expect("parse");
        let mode = match state.mode {
            ProxyMode::Manual(settings) => settings,
            other => panic!("expected manual, got {other:?}"),
        };

        assert_eq!(
            mode.http,
            Some(ProxyEndpoint::new("proxy.local", 7890))
        );
        assert_eq!(
            mode.https,
            Some(ProxyEndpoint::new("secure.local", 7890))
        );
        assert_eq!(
            mode.socks,
            Some(ProxyEndpoint::new("socks.local", 7890))
        );
        assert_eq!(
            mode.bypass.entries,
            vec!["localhost".to_string(), "127.0.0.1".to_string()]
        );
    }

    #[test]
    fn parse_pac_mode() {
        let text = r#"
            <dictionary> {
                ProxyAutoConfigEnable : 1
                ProxyAutoConfigURLString : http://pac.example.com/proxy.pac
                ExceptionsList : {
                    0 : intranet.local
                }
            }
            "#;

        let state = parse_scutil_proxy(text).expect("parse");
        let (url, bypass) = match state.mode {
            ProxyMode::AutoConfigUrl { url, bypass } => (url, bypass),
            other => panic!("expected PAC, got {other:?}"),
        };

        assert_eq!(url, "http://pac.example.com/proxy.pac");
        assert_eq!(bypass.entries, vec!["intranet.local".to_string()]);
    }

    #[test]
    fn parse_auto_discovery() {
        let text = r#"
            <dictionary> {
                ProxyAutoDiscoveryEnable : 1
                ExceptionsList : {
                    0 : *.local
                }
            }
            "#;

        let state = parse_scutil_proxy(text).expect("parse");
        let bypass = match state.mode {
            ProxyMode::AutoDiscovery { bypass } => bypass,
            other => panic!("expected auto discovery, got {other:?}"),
        };

        assert_eq!(bypass.entries, vec!["*.local".to_string()]);
    }

    #[test]
    fn parse_direct_when_empty() {
        let text = r#"
            <dictionary> {
                HTTPEnable : 0
                HTTPSEnable : 0
                SOCKSEnable : 0
            }"#;

        let state = parse_scutil_proxy(text).expect("parse");
        assert!(matches!(state.mode, ProxyMode::Direct));
    }

    #[test]
    fn pac_requires_url() {
        let text = r#"
        <dictionary> {
        ProxyAutoConfigEnable : 1
        }
        "#;

        let err = parse_scutil_proxy(text).expect_err("should fail");
        assert!(matches!(err, Error::Parse(_)));
    }
}
