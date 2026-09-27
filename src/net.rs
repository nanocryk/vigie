use std::net::{IpAddr, Ipv4Addr};

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
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
    let raw = match if_addrs::get_if_addrs() {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<Interface> = Vec::new();
    for iface in raw {
        let IpAddr::V4(v4) = iface.ip() else {
            continue;
        };
        let kind = classify(v4);
        out.push(Interface {
            name: iface.name,
            ip: IpAddr::V4(v4),
            kind,
        });
    }
    out.sort_by(|a, b| {
        (kind_rank(a.kind), a.ip, a.name.clone()).cmp(&(kind_rank(b.kind), b.ip, b.name.clone()))
    });
    out
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
    let o = v4.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

fn kind_rank(kind: IfaceKind) -> u8 {
    match kind {
        IfaceKind::Private => 0,
        IfaceKind::Cgnat => 1,
        IfaceKind::Public => 2,
        IfaceKind::LinkLocal => 3,
        IfaceKind::Loopback => 4,
    }
}
