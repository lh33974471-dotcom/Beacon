use hbb_common::{config::Config, log};
use std::net::IpAddr;

pub const OPTION: &str = "desk";
pub const TOKEN_PREFIX: &str = "desk_";
const MAX_HOPS: i64 = 1;

struct Member {
    id: String,
    position: i64,
    main: bool,
}

fn members(layout: &str) -> Vec<Member> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(layout) else {
        return Vec::new();
    };
    value["devices"]
        .as_array()
        .map(|devices| {
            devices
                .iter()
                .filter_map(|d| {
                    Some(Member {
                        id: d["id"].as_str()?.to_owned(),
                        position: d["position"].as_i64()?,
                        main: d["main"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn store(desk: &serde_json::Value) {
    let value = if desk.is_object() { desk.to_string() } else { String::new() };
    if Config::get_option(OPTION) != value {
        log::info!("Desk: layout {}", if value.is_empty() { "removed" } else { &value });
        Config::set_option(OPTION.to_owned(), value);
    }
}

pub fn is_token(token: &str) -> bool {
    token.starts_with(TOKEN_PREFIX) && token.len() <= 128
}

pub fn switched_off(options: &std::collections::HashMap<String, String>) -> bool {
    let off = |key: &str| options.get(key).map_or(false, |v| v == "N");
    off("enable-kvm") || off("policy-enable-kvm")
}

pub fn side_of(layout: &str, target: &str) -> Option<&'static str> {
    let members = members(layout);
    let main = members.iter().find(|m| m.main)?;
    let other = members.iter().find(|m| m.id == target && !m.main)?;
    let hops = other.position - main.position;
    if hops.abs() > MAX_HOPS || hops == 0 {
        return None;
    }
    Some(if hops < 0 { "left" } else { "right" })
}

pub fn links(layout: &str, me: &str) -> Vec<(String, &'static str)> {
    let members = members(layout);
    if !members.iter().any(|m| m.main && m.id == me) {
        return Vec::new();
    }
    members
        .iter()
        .filter_map(|m| Some((m.id.clone(), side_of(layout, &m.id)?)))
        .collect()
}

pub fn accepts(peer: &str) -> bool {
    let options = Config::get_options();
    if switched_off(&options) {
        return false;
    }
    let layout = options.get(OPTION).cloned().unwrap_or_default();
    let me = Config::get_id();
    let members = members(&layout);
    members.iter().any(|m| m.main && m.id == peer) && side_of(&layout, &me).is_some()
}

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.is_private() || v4.is_link_local(),
            None => {
                let first = v6.segments()[0];
                first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
            }
        },
    }
}
