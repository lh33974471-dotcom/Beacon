//! Beacon LAN-only mode: baked-in fleet password and IP whitelist enforcement.
//! See docs/adr/0001-pure-lan-direct-connection.md and 0002-baked-in-password-over-zero-auth.md.

use hbb_common::config::{keys, Config};
use std::net::IpAddr;

/// Whether this build strips all rendezvous/relay/API dependencies.
pub const LAN_ONLY: bool = true;

/// Fleet-wide connection password compiled into both ends.
/// The controller sends it automatically; users never see it.
/// Injected at build time via the BEACON_FLEET_PASSWORD env var (build.ps1
/// reads it from the untracked .secrets/ folder). Treat it as a gate, not a
/// vault: it can be reverse-engineered from the binary, so keep the
/// whitelist on too. The placeholder below is for public-source builds.
pub const BAKED_PASSWORD: &str = match option_env!("BEACON_FLEET_PASSWORD") {
    Some(p) => p,
    None => "CHANGE-ME-BEFORE-DEPLOY",
};

fn is_lan_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

fn cidr_match_v4(ip: std::net::Ipv4Addr, net: std::net::Ipv4Addr, prefix: u32) -> bool {
    if prefix > 32 {
        return false;
    }
    if prefix == 0 {
        return true;
    }
    let shift = 32 - prefix;
    (u32::from(ip) >> shift) == (u32::from(net) >> shift)
}

/// Whether an incoming peer IP is allowed to connect.
/// If the `whitelist` option is empty, only LAN (private/loopback/link-local)
/// addresses are allowed. Otherwise only listed IPs and IPv4 CIDRs match.
/// Entries are separated by comma, semicolon, space or newline.
pub fn ip_whitelist_allows(ip_str: &str) -> bool {
    let ip: IpAddr = match ip_str.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    let whitelist = Config::get_option(keys::OPTION_WHITELIST);
    if whitelist.trim().is_empty() {
        return is_lan_ip(&ip);
    }
    for entry in whitelist
        .split(|c: char| c == ',' || c == ';' || c == ' ' || c == '\n' || c == '\r' || c == '\t')
        .filter(|s| !s.is_empty())
    {
        if let Some((net, prefix)) = entry.split_once('/') {
            if let (Ok(IpAddr::V4(net)), Ok(prefix)) = (net.parse(), prefix.parse::<u32>()) {
                if let IpAddr::V4(v4) = ip {
                    if cidr_match_v4(v4, net, prefix) {
                        return true;
                    }
                }
            }
        } else if let Ok(entry_ip) = entry.parse::<IpAddr>() {
            if entry_ip == ip {
                return true;
            }
        }
    }
    false
}
