//! Callback URL policy. Hosts that can resolve names must also check the
//! resolved address at connection time; this check covers literal hosts.

use std::net::{Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// Accept only `https` URLs to public hosts. With `allow_local_testing`, also
/// accept `http` or `https` to `localhost` or a loopback literal.
pub fn validate_callback_url(raw: &str, allow_local_testing: bool) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|error| format!("callback URL is malformed: {error}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("callback URL must not carry credentials".into());
    }
    let host = url.host().ok_or("callback URL has no host")?;
    let loopback = match &host {
        Host::Domain(name) => is_localhost(name),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    };
    if allow_local_testing && loopback && matches!(url.scheme(), "http" | "https") {
        return Ok(url);
    }
    if url.scheme() != "https" {
        return Err("callback URL must use https".into());
    }
    let blocked = match host {
        Host::Domain(name) => is_localhost(name),
        Host::Ipv4(address) => is_blocked_v4(address),
        Host::Ipv6(address) => is_blocked_v6(address),
    };
    if blocked {
        return Err("callback URL must name a public host".into());
    }
    Ok(url)
}

fn is_localhost(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost")
}

fn is_blocked_v4(address: Ipv4Addr) -> bool {
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_unspecified()
        || address.octets()[0] == 0
        || address.is_broadcast()
}

fn is_blocked_v6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_blocked_v4(mapped);
    }
    let first = address.segments()[0];
    address.is_loopback()
        || address.is_unspecified()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
}
