//! Client identity ([`PeerInfo`]) for per-client limits.

use std::net::IpAddr;

use axum::http::HeaderMap;

use crate::proxy::PeerInfo;

/// Forwarding headers in trust order; each contributes its first entry.
const FORWARDED_HEADERS: [&str; 3] = ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"];

/// The client identity of a request: the socket address, or with
/// `trust_forwarded` the first valid IP from the forwarding headers.
pub(crate) fn identify(remote: IpAddr, headers: &HeaderMap, trust_forwarded: bool) -> PeerInfo {
    let forwarded = trust_forwarded
        .then(|| {
            FORWARDED_HEADERS
                .iter()
                .find_map(|name| first_ip(headers, name))
        })
        .flatten();
    PeerInfo::new(forwarded.unwrap_or(remote).to_canonical().to_string())
}

fn first_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, axum::http::HeaderValue::from_static(value));
        }
        map
    }

    const REMOTE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    #[test]
    fn socket_address_without_trust() {
        let h = headers(&[("x-forwarded-for", "203.0.113.1")]);
        assert_eq!(identify(REMOTE, &h, false).as_str(), "127.0.0.1");
    }

    #[test]
    fn forwarded_order_and_fallbacks() {
        let h = headers(&[
            ("cf-connecting-ip", "not an ip"),
            ("x-real-ip", "2001:db8::1"),
            ("x-forwarded-for", "203.0.113.1"),
        ]);
        assert_eq!(identify(REMOTE, &h, true).as_str(), "2001:db8::1");
        let h = headers(&[("x-forwarded-for", " 203.0.113.9 , 10.0.0.1")]);
        assert_eq!(identify(REMOTE, &h, true).as_str(), "203.0.113.9");
        assert_eq!(
            identify(REMOTE, &HeaderMap::new(), true).as_str(),
            "127.0.0.1"
        );
    }

    #[test]
    fn mapped_ipv4_is_canonical() {
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap_or(REMOTE);
        assert_eq!(
            identify(mapped, &HeaderMap::new(), false).as_str(),
            "192.0.2.1"
        );
    }
}
