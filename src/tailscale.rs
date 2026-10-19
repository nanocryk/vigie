//! `vigie tailscale`: a certificate for the machine's tailnet name, which every
//! device on the tailnet trusts, wired into `config.json`.

use std::ffi::OsStr;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::state_dir;

const DEFAULT_PORT: u16 = 7879;

/// What `set_up` did.
pub struct Setup {
    pub domain: String,
    pub cert_dir: PathBuf,
    pub bind: SocketAddr,
}

/// Issues the certificate into `dir/tailscale` and points `dir/config.json` at it.
pub fn set_up(dir: &Path) -> Result<Setup> {
    let status = tailscale(&[OsStr::new("status"), OsStr::new("--json")])?;
    let domain = cert_domain(&status)?;
    let cert_dir = dir.join("tailscale");
    state_dir::create_private_dir(&cert_dir)?;
    let cert = cert_dir.join(format!("{domain}.crt"));
    let key = cert_dir.join(format!("{domain}.key"));
    tailscale(&[
        OsStr::new("cert"),
        OsStr::new("--cert-file"),
        cert.as_os_str(),
        OsStr::new("--key-file"),
        key.as_os_str(),
        OsStr::new(&domain),
    ])?;
    state_dir::restrict_to_owner(&key)?;

    let config_path = dir.join("config.json");
    let config = match std::fs::read(&config_path) {
        Ok(bytes) => Some(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", config_path.display()))?,
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("read {}", config_path.display())),
    };
    let config = merged_config(config, &domain, &cert, &key)?;
    let bind = serde_json::from_value(config["bind"].clone()).context("merged bind")?;
    let mut text = serde_json::to_string_pretty(&config)?;
    text.push('\n');
    std::fs::write(&config_path, text)
        .with_context(|| format!("write {}", config_path.display()))?;
    Ok(Setup {
        domain,
        cert_dir,
        bind,
    })
}

/// Runs the `tailscale` CLI, returning its stdout.
fn tailscale(args: &[&OsStr]) -> Result<Vec<u8>> {
    let output = match Command::new("tailscale").args(args).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => bail!(
            "the `tailscale` command is not found: install Tailscale \
             (https://tailscale.com/download) and log in, then try again"
        ),
        Err(err) => return Err(err).context("run tailscale"),
    };
    if !output.status.success() {
        let subcommand = args.first().map_or("".into(), |arg| arg.to_string_lossy());
        bail!(
            "`tailscale {subcommand}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    backend_state: String,
    #[serde(default)]
    cert_domains: Option<Vec<String>>,
}

/// The name `tailscale cert` issues for, read from `tailscale status --json`.
fn cert_domain(status_json: &[u8]) -> Result<String> {
    let status: Status =
        serde_json::from_slice(status_json).context("parse `tailscale status --json`")?;
    if status.backend_state != "Running" {
        bail!(
            "Tailscale is not connected (state {}): log in with `tailscale up`, then try again",
            status.backend_state
        );
    }
    match status.cert_domains.unwrap_or_default().into_iter().next() {
        Some(domain) => Ok(domain),
        None => bail!(
            "HTTPS certificates are off for this tailnet: enable MagicDNS and HTTPS \
             Certificates on the DNS page of the admin console \
             (https://login.tailscale.com/admin/dns), then try again"
        ),
    }
}

/// `config` serving the certificate under the tailnet name, on every interface
/// unless it already binds a non-loopback address.
fn merged_config(config: Option<Value>, domain: &str, cert: &Path, key: &Path) -> Result<Value> {
    let mut object = match config {
        None => Map::new(),
        Some(Value::Object(object)) => object,
        Some(_) => bail!("config.json is not a JSON object"),
    };
    let bind: Option<SocketAddr> = object
        .get("bind")
        .map(|bind| serde_json::from_value(bind.clone()))
        .transpose()
        .context("config.json bind")?;
    if bind.is_none_or(|bind| bind.ip().is_loopback()) {
        let port = bind.map_or(DEFAULT_PORT, |bind| bind.port());
        let every_interface = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
        object.insert("bind".into(), every_interface.to_string().into());
    }
    object.insert("cert".into(), serde_json::to_value(cert)?);
    object.insert("key".into(), serde_json::to_value(key)?);
    object.remove("self_signed");
    let hosts = object
        .entry("public_hosts")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(hosts) = hosts else {
        bail!("config.json public_hosts is not a list");
    };
    if !hosts
        .iter()
        .any(|host| host.as_str().is_some_and(|host| host_name(host) == domain))
    {
        hosts.push(domain.into());
    }
    Ok(Value::Object(object))
}

/// `host` of a `[scheme://]host[:port][/]` entry.
fn host_name(entry: &str) -> &str {
    let entry = entry
        .strip_prefix("https://")
        .or_else(|| entry.strip_prefix("http://"))
        .unwrap_or(entry);
    entry.split([':', '/']).next().unwrap_or(entry)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DOMAIN: &str = "box.tail1234.ts.net";

    #[test]
    fn the_cert_domain_needs_a_running_tailnet_with_https() {
        let running = br#"{"BackendState": "Running", "CertDomains": ["box.tail1234.ts.net"]}"#;
        assert_eq!(cert_domain(running).unwrap(), DOMAIN);
        let logged_out = br#"{"BackendState": "NeedsLogin", "CertDomains": null}"#;
        assert!(
            cert_domain(logged_out)
                .unwrap_err()
                .to_string()
                .contains("tailscale up")
        );
        for no_https in [
            &br#"{"BackendState": "Running", "CertDomains": null}"#[..],
            br#"{"BackendState": "Running"}"#,
        ] {
            assert!(
                cert_domain(no_https)
                    .unwrap_err()
                    .to_string()
                    .contains("HTTPS")
            );
        }
    }

    fn merge(config: Option<Value>) -> Value {
        merged_config(config, DOMAIN, Path::new("c.crt"), Path::new("c.key")).unwrap()
    }

    #[test]
    fn a_missing_config_gets_every_field() {
        assert_eq!(
            merge(None),
            json!({
                "bind": "0.0.0.0:7879",
                "cert": "c.crt",
                "key": "c.key",
                "public_hosts": [DOMAIN],
            })
        );
    }

    #[test]
    fn the_merge_keeps_other_fields_and_a_reachable_bind() {
        let config = json!({
            "bind": "100.101.102.103:9000",
            "cert": "old.crt",
            "key": "old.key",
            "public_hosts": ["other.example", "https://box.tail1234.ts.net:9000/"],
            "allow_origins": ["https://other.example"],
        });
        let merged = merge(Some(config));
        assert_eq!(merged["bind"], "100.101.102.103:9000");
        assert_eq!(merged["cert"], "c.crt");
        assert_eq!(
            merged["public_hosts"],
            json!(["other.example", "https://box.tail1234.ts.net:9000/"])
        );
        assert_eq!(merged["allow_origins"], json!(["https://other.example"]));
    }

    #[test]
    fn a_loopback_bind_opens_on_its_port_and_self_signed_goes() {
        let merged = merge(Some(json!({"bind": "127.0.0.1:9000", "self_signed": true})));
        assert_eq!(merged["bind"], "0.0.0.0:9000");
        assert!(merged.get("self_signed").is_none());
    }

    #[test]
    fn a_malformed_config_is_refused() {
        let call = |config| merged_config(Some(config), DOMAIN, Path::new("c"), Path::new("k"));
        assert!(call(json!([])).is_err());
        assert!(call(json!({"bind": "nowhere"})).is_err());
        assert!(call(json!({"public_hosts": "box"})).is_err());
    }
}
