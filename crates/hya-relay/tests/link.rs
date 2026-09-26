#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Relay link grammar:
//! `hya[+insecure]://<host>[:port][/<prefix>]/<room_id>[?t=auto|grpc|ws]#<b64url(x25519_pub)>.<b64url(psk)>`.

use hya_relay::link::{
    KeyField, LinkError, RelayAddress, RelayLink, RoomId, Transport, WsRoute, room_id_from_ed25519,
};

const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"; // bytes 0..32
const PSK_B64: &str = "__________________________________________8"; // 0xff * 32
const ROOM: &str = "eh7ddx5bksrgcytl7bkai36se4";

fn key() -> [u8; 32] {
    std::array::from_fn(|i| i as u8)
}

type ErrorCheck = fn(&LinkError) -> bool;

fn link(rest: &str) -> String {
    format!("{rest}#{KEY_B64}.{PSK_B64}")
}

#[test]
fn room_id_derivation_matches_reference_vector() {
    // RFC 8032 test 1 public key; expected value computed independently as
    // base32(sha256(pk)).lower().rstrip("=")[:26].
    let pk: [u8; 32] = [
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07,
        0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07,
        0x51, 0x1a,
    ];
    assert_eq!(room_id_from_ed25519(&pk).as_str(), ROOM);
    assert_eq!(
        RoomId::from_ed25519(&[0; 32]).as_str(),
        "mzuhvlpymk6xo3epygfy5h4oea"
    );
}

#[test]
fn room_id_parse_validates_alphabet_and_length() {
    assert_eq!(RoomId::parse(ROOM).unwrap().as_str(), ROOM);
    for bad in [
        "",
        "short",
        "EH7DDX5BKSRGCYTL7BKAI36SE4",
        "eh7ddx5bksrgcytl7bkai36se1",
        "eh7ddx5bksrgcytl7bkai36se4a",
    ] {
        assert!(
            matches!(RoomId::parse(bad), Err(LinkError::InvalidRoomId(_))),
            "{bad}"
        );
    }
}

#[test]
fn full_link_round_trips() {
    let text = link(&format!(
        "hya://relay.example.com:8443/hya/relay/{ROOM}?t=ws"
    ));
    let parsed: RelayLink = text.parse().unwrap();
    assert!(parsed.address().is_secure());
    assert_eq!(parsed.address().host(), "relay.example.com");
    assert_eq!(parsed.address().port(), 8443);
    assert_eq!(parsed.address().prefix(), "/hya/relay");
    assert_eq!(parsed.room_id().as_str(), ROOM);
    assert_eq!(parsed.transport(), Transport::Ws);
    assert_eq!(parsed.server_key(), &key());
    assert_eq!(parsed.psk(), &[0xff; 32]);
    assert_eq!(parsed.to_secret_string(), text);
    let again: RelayLink = parsed.to_secret_string().parse().unwrap();
    assert_eq!(again, parsed);
}

#[test]
fn defaults_apply_and_are_omitted_when_formatting() {
    let secure: RelayLink = link(&format!("hya://relay.example.com/{ROOM}"))
        .parse()
        .unwrap();
    assert_eq!(secure.address().port(), 443);
    assert_eq!(secure.address().prefix(), "");
    assert_eq!(secure.transport(), Transport::Auto);
    assert_eq!(
        secure.to_secret_string(),
        link(&format!("hya://relay.example.com/{ROOM}"))
    );

    let insecure: RelayLink = link(&format!("hya+insecure://100.64.0.7/{ROOM}"))
        .parse()
        .unwrap();
    assert!(!insecure.address().is_secure());
    assert_eq!(insecure.address().port(), 80);

    // Explicit default port and explicit t=auto canonicalize away.
    let explicit: RelayLink = link(&format!("hya://Relay.Example.com:443/{ROOM}?t=auto"))
        .parse()
        .unwrap();
    assert_eq!(explicit, secure);
}

#[test]
fn insecure_link_with_port_and_ipv6_host() {
    let text = link(&format!("hya+insecure://[fd7a:115c::1]:8766/{ROOM}?t=grpc"));
    let parsed: RelayLink = text.parse().unwrap();
    assert_eq!(parsed.address().host(), "[fd7a:115c::1]");
    assert_eq!(parsed.address().port(), 8766);
    assert_eq!(parsed.transport(), Transport::Grpc);
    assert_eq!(parsed.to_secret_string(), text);
    assert_eq!(parsed.address().base_url(), "http://[fd7a:115c::1]:8766");
}

#[test]
fn transport_parameter_parsing() {
    for (t, want) in [
        ("auto", Transport::Auto),
        ("grpc", Transport::Grpc),
        ("ws", Transport::Ws),
    ] {
        let parsed: RelayLink = link(&format!("hya://h.example/{ROOM}?t={t}"))
            .parse()
            .unwrap();
        assert_eq!(parsed.transport(), want);
        assert_eq!(want.as_str(), t);
    }
    let err = link(&format!("hya://h.example/{ROOM}?t=quic"))
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::UnknownTransport(ref t) if t == "quic"));
    // Unknown parameters are ignored for forward compatibility.
    let parsed: RelayLink = link(&format!("hya://h.example/{ROOM}?v=2&t=ws"))
        .parse()
        .unwrap();
    assert_eq!(parsed.transport(), Transport::Ws);
    // A repeated t is ambiguous.
    let err = link(&format!("hya://h.example/{ROOM}?t=ws&t=grpc"))
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidQuery(_)));
}

#[test]
fn unknown_scheme_is_rejected() {
    for text in [
        link(&format!("https://h.example/{ROOM}")),
        link(&format!("hya+h2c://h.example/{ROOM}")),
        link(&format!("h.example/{ROOM}")),
    ] {
        assert!(
            matches!(text.parse::<RelayLink>(), Err(LinkError::UnknownScheme(_))),
            "{text}"
        );
    }
}

#[test]
fn missing_or_malformed_fragment_is_rejected() {
    let err = format!("hya://h.example/{ROOM}")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::MissingFragment));
    let err = format!("hya://h.example/{ROOM}#")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::MissingFragment));
    let err = format!("hya://h.example/{ROOM}#{KEY_B64}")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidFragment));
    let err = format!("hya://h.example/{ROOM}#{KEY_B64}.{PSK_B64}.x")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidFragment));
    let err = format!("hya://h.example/{ROOM}#{KEY_B64}.{PSK_B64}=")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(
        err,
        LinkError::InvalidBase64 {
            field: KeyField::Psk
        }
    ));
    let err = format!("hya://h.example/{ROOM}#!!.{PSK_B64}")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(
        err,
        LinkError::InvalidBase64 {
            field: KeyField::ServerKey
        }
    ));
}

#[test]
fn bad_key_length_is_rejected() {
    // 31 bytes of server key.
    let short = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHg";
    let err = format!("hya://h.example/{ROOM}#{short}.{PSK_B64}")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(
        err,
        LinkError::InvalidKeyLength {
            field: KeyField::ServerKey,
            len: 31
        }
    ));
    let err = format!("hya://h.example/{ROOM}#{KEY_B64}.AAAA")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(
        err,
        LinkError::InvalidKeyLength {
            field: KeyField::Psk,
            len: 3
        }
    ));
}

#[test]
fn authority_and_path_errors() {
    let cases: [(String, ErrorCheck); 7] = [
        (link("hya:///room"), |e| {
            matches!(e, LinkError::InvalidHost(_))
        }),
        (link(&format!("hya://user@h.example/{ROOM}")), |e| {
            matches!(e, LinkError::InvalidHost(_))
        }),
        (link(&format!("hya://h.example:0/{ROOM}")), |e| {
            matches!(e, LinkError::InvalidPort(_))
        }),
        (link(&format!("hya://h.example:99999/{ROOM}")), |e| {
            matches!(e, LinkError::InvalidPort(_))
        }),
        (link("hya://h.example"), |e| {
            matches!(e, LinkError::MissingRoom)
        }),
        (link("hya://h.example/"), |e| {
            matches!(e, LinkError::MissingRoom)
        }),
        (link(&format!("hya://h.example/a//{ROOM}")), |e| {
            matches!(e, LinkError::InvalidPath(_))
        }),
    ];
    for (text, check) in cases {
        let err = text.parse::<RelayLink>().unwrap_err();
        assert!(check(&err), "{text}: {err:?}");
    }
    let err = link("hya://h.example/NOTAROOM")
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidRoomId(_)));
    let err = link(&format!("hya://h.example/../{ROOM}"))
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidPath(_)));
}

#[test]
fn redaction_hides_the_secret() {
    let text = link(&format!("hya://relay.example.com/hya/{ROOM}?t=grpc"));
    let parsed: RelayLink = text.parse().unwrap();
    let redacted = parsed.redacted();
    assert_eq!(redacted, format!("hya://relay.example.com/hya/{ROOM}"));
    for shown in [redacted, parsed.to_string(), format!("{parsed:?}")] {
        assert!(!shown.contains(KEY_B64), "{shown}");
        assert!(!shown.contains(PSK_B64), "{shown}");
        assert!(!shown.contains('#'), "{shown}");
    }
    // Errors never echo key material either.
    let err = format!("hya://h.example/{ROOM}#{KEY_B64}.{PSK_B64}x").parse::<RelayLink>();
    let message = err.unwrap_err().to_string();
    assert!(
        !message.contains(KEY_B64) && !message.contains(PSK_B64),
        "{message}"
    );
}

#[test]
fn binding_endpoint_urls() {
    let parsed: RelayLink = link(&format!("hya://relay.example.com/hya/{ROOM}"))
        .parse()
        .unwrap();
    let address = parsed.address();
    assert_eq!(address.origin(), "https://relay.example.com");
    assert_eq!(address.base_url(), "https://relay.example.com/hya");
    assert_eq!(address.grpc_url(), "https://relay.example.com/hya");
    assert_eq!(
        address.ws_url(WsRoute::Host),
        "wss://relay.example.com/hya/hya.relay.v1/ws/host"
    );
    assert_eq!(
        address.ws_url(WsRoute::Open),
        "wss://relay.example.com/hya/hya.relay.v1/ws/open"
    );

    let plain: RelayLink = link(&format!("hya+insecure://10.0.0.2:8766/{ROOM}"))
        .parse()
        .unwrap();
    assert_eq!(plain.address().grpc_url(), "http://10.0.0.2:8766");
    assert_eq!(
        plain.address().ws_url(WsRoute::Accept),
        "ws://10.0.0.2:8766/hya.relay.v1/ws/accept"
    );
}

#[test]
fn proxy_url_parses_into_an_address() {
    let address = RelayAddress::parse_proxy_url("https://relay.example.com/hya/").unwrap();
    assert!(address.is_secure());
    assert_eq!(address.port(), 443);
    assert_eq!(address.prefix(), "/hya");
    let address = RelayAddress::parse_proxy_url("http://127.0.0.1:8766").unwrap();
    assert!(!address.is_secure());
    assert_eq!(address.port(), 8766);
    assert_eq!(address.prefix(), "");
    assert!(matches!(
        RelayAddress::parse_proxy_url("ftp://x.example"),
        Err(LinkError::UnknownScheme(_))
    ));
    assert!(matches!(
        RelayAddress::parse_proxy_url("https://x.example/a?b"),
        Err(LinkError::InvalidPath(_))
    ));
}

#[test]
fn link_is_built_from_parts() {
    let address = RelayAddress::parse_proxy_url("https://relay.example.com").unwrap();
    let room = room_id_from_ed25519(&[0; 32]);
    let built = RelayLink::new(address, room, Transport::Auto, key(), [0xff; 32]);
    assert_eq!(
        built.to_secret_string(),
        link("hya://relay.example.com/mzuhvlpymk6xo3epygfy5h4oea")
    );
}
