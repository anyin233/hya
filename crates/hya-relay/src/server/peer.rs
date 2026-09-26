//! Client identity ([`PeerInfo`]) for per-client limits.

use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::str::FromStr;

use axum::http::HeaderMap;

use crate::proxy::PeerInfo;

/// The one forwarding header a proxy behind a trusted hop reads the client
/// address from (`hya proxy --trust-forwarded <header>`).
///
/// Name exactly the header your hop *sets* (overwriting whatever the client
/// sent); every other forwarding header is ignored, so a client cannot pick
/// a more favorable one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForwardedHeader {
    /// `CF-Connecting-IP` (Cloudflare, Cloudflare Tunnel): one address.
    CfConnectingIp,
    /// `X-Real-IP` (nginx `proxy_set_header X-Real-IP $remote_addr`): one
    /// address.
    XRealIp,
    /// `X-Forwarded-For`: the **rightmost** entry, the one the trusted hop
    /// appended (entries to its left come from the client and are ignored).
    XForwardedFor,
}

impl ForwardedHeader {
    /// Every accepted header, in `--trust-forwarded` spelling order.
    pub const ALL: [ForwardedHeader; 3] = [
        ForwardedHeader::CfConnectingIp,
        ForwardedHeader::XRealIp,
        ForwardedHeader::XForwardedFor,
    ];

    /// The lowercase header name (`cf-connecting-ip`, `x-real-ip`,
    /// `x-forwarded-for`), also the `--trust-forwarded` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ForwardedHeader::CfConnectingIp => "cf-connecting-ip",
            ForwardedHeader::XRealIp => "x-real-ip",
            ForwardedHeader::XForwardedFor => "x-forwarded-for",
        }
    }
}

impl fmt::Display for ForwardedHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An unknown `--trust-forwarded` header name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown forwarded header {0:?} (expected cf-connecting-ip, x-real-ip, or x-forwarded-for)"
)]
pub struct UnknownForwardedHeader(pub String);

impl FromStr for ForwardedHeader {
    type Err = UnknownForwardedHeader;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        ForwardedHeader::ALL
            .into_iter()
            .find(|header| header.as_str().eq_ignore_ascii_case(text.trim()))
            .ok_or_else(|| UnknownForwardedHeader(text.to_owned()))
    }
}

/// The client identity of a request: the socket address, or with a trusted
/// header the address it names (falling back to the socket address when the
/// header is missing or malformed). IPv6 addresses are bucketed by /64.
pub(crate) fn identify(
    remote: IpAddr,
    headers: &HeaderMap,
    trusted: Option<ForwardedHeader>,
) -> PeerInfo {
    let forwarded = trusted.and_then(|header| forwarded_ip(headers, header));
    PeerInfo::new(bucket(forwarded.unwrap_or(remote)))
}

/// The address `header` names: its only value, or for `X-Forwarded-For` the
/// rightmost entry of the last header line.
fn forwarded_ip(headers: &HeaderMap, header: ForwardedHeader) -> Option<IpAddr> {
    let value = headers.get_all(header.as_str()).iter().next_back()?;
    let value = value.to_str().ok()?;
    let entry = match header {
        ForwardedHeader::XForwardedFor => value.rsplit(',').next()?,
        ForwardedHeader::CfConnectingIp | ForwardedHeader::XRealIp => value,
    };
    entry.trim().parse().ok()
}

/// The limit bucket of an address: an IPv4 address (IPv4-mapped IPv6
/// included) as is, an IPv6 address as its /64 (`2001:db8:1:2::/64`),
/// since a single host commonly holds a whole /64.
fn bucket(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            format!("{}/64", Ipv6Addr::from(segments))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, axum::http::HeaderValue::from_static(value));
        }
        map
    }

    const REMOTE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    fn id(remote: IpAddr, h: &HeaderMap, trusted: Option<ForwardedHeader>) -> String {
        identify(remote, h, trusted).as_str().to_owned()
    }

    #[test]
    fn socket_address_without_trust() {
        let h = headers(&[
            ("x-forwarded-for", "203.0.113.1"),
            ("cf-connecting-ip", "203.0.113.2"),
            ("x-real-ip", "203.0.113.3"),
        ]);
        assert_eq!(id(REMOTE, &h, None), "127.0.0.1");
    }

    #[test]
    fn only_the_named_header_is_read() {
        let h = headers(&[
            ("cf-connecting-ip", "203.0.113.2"),
            ("x-real-ip", "203.0.113.3"),
            ("x-forwarded-for", "203.0.113.4"),
        ]);
        let cf = Some(ForwardedHeader::CfConnectingIp);
        let real = Some(ForwardedHeader::XRealIp);
        let xff = Some(ForwardedHeader::XForwardedFor);
        assert_eq!(id(REMOTE, &h, cf), "203.0.113.2");
        assert_eq!(id(REMOTE, &h, real), "203.0.113.3");
        assert_eq!(id(REMOTE, &h, xff), "203.0.113.4");
        // A client-supplied other header never substitutes a missing one.
        let only_xff = headers(&[("x-forwarded-for", "203.0.113.9")]);
        assert_eq!(id(REMOTE, &only_xff, cf), "127.0.0.1");
        assert_eq!(id(REMOTE, &only_xff, real), "127.0.0.1");
    }

    #[test]
    fn x_forwarded_for_takes_the_rightmost_entry() {
        let xff = Some(ForwardedHeader::XForwardedFor);
        // The client forged the left entries; the trusted hop appended the
        // last one.
        let h = headers(&[("x-forwarded-for", "10.9.9.9, 203.0.113.1 , 198.51.100.7")]);
        assert_eq!(id(REMOTE, &h, xff), "198.51.100.7");
        // Several header lines: the last line's last entry.
        let h = headers(&[
            ("x-forwarded-for", "10.9.9.9"),
            ("x-forwarded-for", "10.8.8.8, 198.51.100.8"),
        ]);
        assert_eq!(id(REMOTE, &h, xff), "198.51.100.8");
        // Malformed rightmost entry: the socket address, never a left one.
        let h = headers(&[("x-forwarded-for", "203.0.113.1, garbage")]);
        assert_eq!(id(REMOTE, &h, xff), "127.0.0.1");
        assert_eq!(id(REMOTE, &HeaderMap::new(), xff), "127.0.0.1");
    }

    #[test]
    fn ipv6_peers_are_bucketed_by_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().unwrap_or(REMOTE);
        let b: IpAddr = "2001:db8:1:2::1".parse().unwrap_or(REMOTE);
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap_or(REMOTE);
        let none = HeaderMap::new();
        assert_eq!(id(a, &none, None), "2001:db8:1:2::/64");
        assert_eq!(id(a, &none, None), id(b, &none, None));
        assert_ne!(id(a, &none, None), id(c, &none, None));
        // Forwarded IPv6 addresses too.
        let h = headers(&[("x-real-ip", "2001:db8:1:2::ffff")]);
        assert_eq!(
            id(REMOTE, &h, Some(ForwardedHeader::XRealIp)),
            "2001:db8:1:2::/64"
        );
    }

    #[test]
    fn mapped_ipv4_is_canonical() {
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap_or(REMOTE);
        assert_eq!(id(mapped, &HeaderMap::new(), None), "192.0.2.1");
    }

    #[test]
    fn header_names_parse() {
        for header in ForwardedHeader::ALL {
            assert_eq!(header.as_str().parse::<ForwardedHeader>(), Ok(header));
        }
        assert_eq!(
            "X-Forwarded-For".parse::<ForwardedHeader>(),
            Ok(ForwardedHeader::XForwardedFor)
        );
        assert!("forwarded".parse::<ForwardedHeader>().is_err());
        assert!("true".parse::<ForwardedHeader>().is_err());
    }
}
