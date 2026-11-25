#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Relay link grammar:
//! `hya[+insecure]://<host>[:port][/<prefix>]/<room_id>[?t=auto|grpc|ws]#<b64url(x25519_pub)>.<b64url(psk)>`.

use hya_relay::link::{KeyField, LinkError, RelayAddress, RelayLink, RoomId, Transport, WsRoute};

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn room_id_derivation_and_validation() {
    // RFC 8032 test 1 public key; base32(sha256(pk)).lower().rstrip("=")[:26].
    let pk: [u8; 32] = [
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07,
        0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07,
        0x51, 0x1a,
    ];
    assert_eq!(RoomId::from_ed25519(&pk).as_str(), ROOM);
    assert_eq!(
        RoomId::from_ed25519(&[0; 32]).as_str(),
        "mzuhvlpymk6xo3epygfy5h4oea"
    );
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
fn link_parsing_defaults_ipv6_construction_and_public_part() {
    // Full link round-trip.
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
    assert_eq!(
        parsed.to_secret_string().parse::<RelayLink>().unwrap(),
        parsed
    );

    // Defaults apply and canonicalize away.
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
    let explicit: RelayLink = link(&format!("hya://Relay.Example.com:443/{ROOM}?t=auto"))
        .parse()
        .unwrap();
    assert_eq!(explicit, secure);

    let insecure: RelayLink = link(&format!("hya+insecure://100.64.0.7/{ROOM}"))
        .parse()
        .unwrap();
    assert!(!insecure.address().is_secure());
    assert_eq!(insecure.address().port(), 80);

    // Insecure IPv6 with port.
    let ipv6_text = link(&format!("hya+insecure://[fd7a:115c::1]:8766/{ROOM}?t=grpc"));
    let ipv6: RelayLink = ipv6_text.parse().unwrap();
    assert_eq!(ipv6.address().host(), "[fd7a:115c::1]");
    assert_eq!(ipv6.address().port(), 8766);
    assert_eq!(ipv6.transport(), Transport::Grpc);
    assert_eq!(ipv6.to_secret_string(), ipv6_text);
    assert_eq!(ipv6.address().base_url(), "http://[fd7a:115c::1]:8766");

    // Constructed from parts.
    let address = RelayAddress::parse_proxy_url("https://relay.example.com").unwrap();
    let built = RelayLink::new(
        address,
        RoomId::from_ed25519(&[0; 32]),
        Transport::Auto,
        key(),
        [0xff; 32],
    );
    assert_eq!(
        built.to_secret_string(),
        link("hya://relay.example.com/mzuhvlpymk6xo3epygfy5h4oea")
    );

    // Public part parses without a secret (ignoring any fragment).
    let (addr, room, transport) =
        RelayLink::parse_public(&format!("hya://relay.example.com:8443/hya/{ROOM}?t=ws")).unwrap();
    assert_eq!(addr.base_url(), "https://relay.example.com:8443/hya");
    assert_eq!(room.as_str(), ROOM);
    assert_eq!(transport, Transport::Ws);
    let (_, again, _) =
        RelayLink::parse_public(&format!("hya://relay.example.com/{ROOM}#junk")).unwrap();
    assert_eq!(again.as_str(), ROOM);
    assert!(RelayLink::parse_public("hya://relay.example.com/").is_err());
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
    let parsed: RelayLink = link(&format!("hya://h.example/{ROOM}?v=2&t=ws"))
        .parse()
        .unwrap();
    assert_eq!(parsed.transport(), Transport::Ws);
    let err = link(&format!("hya://h.example/{ROOM}?t=ws&t=grpc"))
        .parse::<RelayLink>()
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidQuery(_)));
}

#[test]
fn link_rejection_matrix() {
    let short_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHg"; // 31 bytes
    let cases: Vec<(String, ErrorCheck)> = vec![
        // Schemes
        (link(&format!("https://h.example/{ROOM}")), |e| {
            matches!(e, LinkError::UnknownScheme(_))
        }),
        (link(&format!("hya+h2c://h.example/{ROOM}")), |e| {
            matches!(e, LinkError::UnknownScheme(_))
        }),
        (link(&format!("h.example/{ROOM}")), |e| {
            matches!(e, LinkError::UnknownScheme(_))
        }),
        // Fragment / base64 / key lengths
        (format!("hya://h.example/{ROOM}"), |e| {
            matches!(e, LinkError::MissingFragment)
        }),
        (format!("hya://h.example/{ROOM}#"), |e| {
            matches!(e, LinkError::MissingFragment)
        }),
        (format!("hya://h.example/{ROOM}#{KEY_B64}"), |e| {
            matches!(e, LinkError::InvalidFragment)
        }),
        (
            format!("hya://h.example/{ROOM}#{KEY_B64}.{PSK_B64}.x"),
            |e| matches!(e, LinkError::InvalidFragment),
        ),
        (
            format!("hya://h.example/{ROOM}#{KEY_B64}.{PSK_B64}="),
            |e| {
                matches!(
                    e,
                    LinkError::InvalidBase64 {
                        field: KeyField::Psk
                    }
                )
            },
        ),
        (format!("hya://h.example/{ROOM}#!!.{PSK_B64}"), |e| {
            matches!(
                e,
                LinkError::InvalidBase64 {
                    field: KeyField::ServerKey
                }
            )
        }),
        (
            format!("hya://h.example/{ROOM}#{short_key}.{PSK_B64}"),
            |e| {
                matches!(
                    e,
                    LinkError::InvalidKeyLength {
                        field: KeyField::ServerKey,
                        len: 31
                    }
                )
            },
        ),
        (format!("hya://h.example/{ROOM}#{KEY_B64}.AAAA"), |e| {
            matches!(
                e,
                LinkError::InvalidKeyLength {
                    field: KeyField::Psk,
                    len: 3
                }
            )
        }),
        // Authority & path
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
        (link("hya://h.example/NOTAROOM"), |e| {
            matches!(e, LinkError::InvalidRoomId(_))
        }),
        (link(&format!("hya://h.example/../{ROOM}")), |e| {
            matches!(e, LinkError::InvalidPath(_))
        }),
    ];
    for (text, check) in cases {
        let err = text.parse::<RelayLink>().unwrap_err();
        assert!(check(&err), "{text}: {err:?}");
    }
}

#[test]
fn redaction_and_errors_never_echo_secrets() {
    let text = link(&format!("hya://relay.example.com/hya/{ROOM}?t=grpc"));
    let parsed: RelayLink = text.parse().unwrap();
    let redacted = parsed.redacted();
    assert_eq!(redacted, format!("hya://relay.example.com/hya/{ROOM}"));
    for shown in [redacted, parsed.to_string(), format!("{parsed:?}")] {
        assert!(!shown.contains(KEY_B64), "{shown}");
        assert!(!shown.contains(PSK_B64), "{shown}");
        assert!(!shown.contains('#'), "{shown}");
    }
    let err = format!("hya://h.example/{ROOM}#{KEY_B64}.{PSK_B64}x")
        .parse::<RelayLink>()
        .unwrap_err()
        .to_string();
    assert!(!err.contains(KEY_B64) && !err.contains(PSK_B64), "{err}");

    const SECRET: &str = "S3CR3TPSKMATERIAL";
    for input in [
        format!("https://relay.example.com#{SECRET}"),
        format!("https://relay.example.com/a#{SECRET}"),
        format!("https://relay.example.com/{ROOM}?t=ws#{SECRET}.{SECRET}"),
        format!("http://relay.example.com/bad path/x#{SECRET}"),
        format!("hya://relay.example.com/bad path/{ROOM}#{SECRET}"),
        format!("hya://relay.example.com/a//{ROOM}#{SECRET}.{SECRET}"),
        format!("hya://relay.example.com/{ROOM}#{SECRET}"),
        format!("hya://relay.example.com/{ROOM}/#{SECRET}.x"),
    ] {
        for error in [
            RelayAddress::parse_proxy_url(&input).err(),
            RelayLink::parse(&input).err(),
        ]
        .into_iter()
        .flatten()
        {
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains(SECRET), "{input} -> {shown}");
        }
    }
}

#[test]
fn binding_endpoints_and_proxy_url_parsing() {
    let parsed: RelayLink = link(&format!("hya://relay.example.com/hya/{ROOM}"))
        .parse()
        .unwrap();
    let address = parsed.address();
    assert_eq!(address.origin(), "https://relay.example.com");
    assert_eq!(address.base_url(), "https://relay.example.com/hya");
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
    assert_eq!(plain.address().base_url(), "http://10.0.0.2:8766");
    assert_eq!(
        plain.address().ws_url(WsRoute::Accept),
        "ws://10.0.0.2:8766/hya.relay.v1/ws/accept"
    );

    let addr = RelayAddress::parse_proxy_url("https://relay.example.com/hya/").unwrap();
    assert!(addr.is_secure() && addr.port() == 443 && addr.prefix() == "/hya");
    let addr = RelayAddress::parse_proxy_url("http://127.0.0.1:8766").unwrap();
    assert!(!addr.is_secure() && addr.port() == 8766 && addr.prefix().is_empty());
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
fn open_token_hmac_derivation_and_link_integration() {
    use hya_relay::keys::{OpenToken, Psk, open_token_hash};
    let room = RoomId::from_ed25519(&[1; 32]);
    assert_eq!(room.as_str(), "olgw5bbcyqd7w3ijq2ipceylpx");
    let token = OpenToken::derive(&Psk::from_bytes([7; 32]), &room);
    assert_eq!(
        hex(token.as_bytes()),
        "8142fdb28d7bd5e3785a349374df31ebe01e65f0987ab9f60a8655a4071e80cb"
    );
    assert_eq!(
        hex(&token.hash()),
        "01663e0e9ae0d88ee4907b6e08bb17d45aca0cc28e739151e32606227214981c"
    );
    assert_eq!(open_token_hash(token.as_bytes()), token.hash());
    assert_ne!(
        OpenToken::derive(&Psk::from_bytes([8; 32]), &room).as_bytes(),
        token.as_bytes()
    );
    assert_ne!(
        OpenToken::derive(&Psk::from_bytes([7; 32]), &RoomId::from_ed25519(&[2; 32])).as_bytes(),
        token.as_bytes()
    );
    assert!(!format!("{token:?}").contains(&hex(token.as_bytes())));

    let parsed = RelayLink::parse(&link(&format!("hya://relay.example.com/{ROOM}"))).unwrap();
    let psk = parsed.psk_key();
    assert_eq!(psk.as_bytes(), &[0xff; 32]);
    let expected = OpenToken::derive(&psk, parsed.room_id());
    assert_eq!(parsed.open_token().as_bytes(), expected.as_bytes());
    let from_keys = RelayLink::from_keys(
        parsed.address().clone(),
        parsed.room_id().clone(),
        Transport::Auto,
        *parsed.server_key(),
        &psk,
    );
    assert_eq!(from_keys, parsed);
}
