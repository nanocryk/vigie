use std::net::{IpAddr, Ipv4Addr};

use serde::Serialize;

/// Declared in the order interfaces are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IfaceKind {
    Private,
    Cgnat,
    Public,
    LinkLocal,
    Loopback,
}

#[derive(Debug, Clone, Serialize)]
pub struct Interface {
    pub name: String,
    pub ip: IpAddr,
    pub kind: IfaceKind,
}

/// Every IPv4 address the host owns, tagged by class. Loopback is included so
/// the UI can offer it explicitly; callers filter as needed.
pub fn enumerate() -> Vec<Interface> {
    let Ok(raw) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut interfaces: Vec<Interface> = raw
        .into_iter()
        .filter_map(|iface| match iface.ip() {
            IpAddr::V4(v4) => Some(Interface {
                name: iface.name,
                ip: IpAddr::V4(v4),
                kind: classify(v4),
            }),
            IpAddr::V6(_) => None,
        })
        .collect();
    interfaces.sort_by(|left, right| {
        (left.kind, left.ip, &left.name).cmp(&(right.kind, right.ip, &right.name))
    });
    interfaces
}

/// Non-loopback IPv4 addresses this host can be reached on. Used at startup
/// for the TLS SAN set and the pair URL list. Deliberately broad: private,
/// CGNAT (Tailscale-ish), and public all included so pairing choices at UI
/// time don't require a cert regeneration + restart.
pub fn all_reachable_ipv4() -> Vec<IpAddr> {
    enumerate()
        .into_iter()
        .filter(|iface| !matches!(iface.kind, IfaceKind::Loopback | IfaceKind::LinkLocal))
        .map(|iface| iface.ip)
        .collect()
}

/// `scheme://host`, with `:port` unless `host` already names one.
pub fn origin(scheme: &str, host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    }
}

fn classify(v4: Ipv4Addr) -> IfaceKind {
    if v4.is_loopback() {
        IfaceKind::Loopback
    } else if v4.is_link_local() {
        IfaceKind::LinkLocal
    } else if v4.is_private() {
        IfaceKind::Private
    } else if is_cgnat(v4) {
        IfaceKind::Cgnat
    } else {
        IfaceKind::Public
    }
}

fn is_cgnat(v4: Ipv4Addr) -> bool {
    let octets = v4.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}
