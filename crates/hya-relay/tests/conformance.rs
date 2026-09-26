#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Intermediary conformance: a real relay server behind in-process hops
//! that behave like the HTTPS intermediaries the relay must work through.
//! Every case runs a backend (host control stream with reconnects, Noise
//! responder, echo) and a client (open, Noise initiator) through the relay
//! client and checks a round trip.
//!
//! | Case | Hop | Deployment shape |
//! | --- | --- | --- |
//! | (a) | h2c-capable HTTP reverse proxy | nginx `grpc_pass`, Caddy `h2c://` |
//! | (b) | TLS on the relay, private CA | direct TLS, `tailscale serve` |
//! | (c) | HTTP/1.1-only proxy forwarding WS upgrades | Cloudflare Tunnel, nginx without h2 |
//! | (d) | h2 proxy that drops trailers | misconfigured h2 proxies/CDNs |
//! | (e) | cuts connections idle for N ms | nginx/Cloudflare idle timeouts |
//! | (f) | cuts every connection after N ms | max stream duration caps |
//! | (g) | routes only a path prefix | shared hosts, path-routed ingress |
//! | (h) | rewrites Host / `:authority` | Cloudflare Tunnel, Caddy, `tailscale serve` |
//! | (i) | none, `hya+insecure://` | tailnet |

mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use hya_relay::client::{
    Binding, ChoiceReason, ClientConfig, ClientError, HeartbeatConfig, ProbeFailureKind,
    RelayClient,
};
use hya_relay::link::{RelayAddress, RelayLink, Transport};
use tokio::time::{sleep, timeout};

use support::hops::{HttpHop, HttpHopOptions, TcpHop, TcpHopOptions};
use support::{Backend, Identity, Relay, TestTls, WAIT, ping, plain, quick_policy, round_trip};

fn config(transport: Transport) -> ClientConfig {
    ClientConfig {
        transport,
        connect_timeout: Duration::from_secs(5),
        probe_timeout: Duration::from_secs(5),
        open_timeout: Duration::from_secs(5),
        heartbeat: HeartbeatConfig::every(Duration::from_millis(50))
            .dead_peer_after(Duration::from_secs(2)),
        ..ClientConfig::default()
    }
}

fn client(address: &RelayAddress, transport: Transport) -> RelayClient {
    RelayClient::new(address.clone(), config(transport)).unwrap()
}

/// A backend on `address` (auto binding) and a round trip from a client of
/// each binding in `transports`. Returns the auto client's binding choice.
async fn splice_through(address: &RelayAddress, transports: &[Transport]) -> Vec<Binding> {
    let identity = Identity::new(1);
    let link = identity.link(address.clone(), Transport::Auto);
    let backend = Backend::start(client(address, Transport::Auto), identity, quick_policy()).await;
    let mut chosen = Vec::new();
    for &transport in transports {
        let opener = client(address, transport);
        round_trip(&opener, &link, format!("hello over {transport}").as_bytes()).await;
        chosen.push(opener.binding().await.unwrap().binding);
    }
    drop(backend);
    chosen
}

const ALL: [Transport; 3] = [Transport::Auto, Transport::Grpc, Transport::Ws];

// (a) An h2c-capable reverse proxy passes both bindings; auto picks gRPC.
#[tokio::test]
async fn a_h2c_hop_carries_both_bindings_and_auto_picks_grpc() {
    let relay = Relay::start(support::relay_config()).await;
    let hop = HttpHop::start(relay.addr, HttpHopOptions::default()).await;
    let address = plain(hop.addr, "");
    let chosen = splice_through(&address, &ALL).await;
    assert_eq!(chosen, [Binding::Grpc, Binding::Grpc, Binding::Ws]);
    assert!(hop.forwarded.load(Ordering::SeqCst) > 0);
    relay.stop().await;
}

// (b) TLS on the relay with a private CA: SNI = link host, ALPN h2 for gRPC
// and http/1.1 for WebSocket.
#[tokio::test]
async fn b_tls_with_an_extra_ca_carries_both_bindings() {
    let tls = TestTls::new();
    let relay = Relay::start(support::relay_config().tls(tls.files.clone())).await;
    let address = RelayAddress::new(true, "localhost", Some(relay.addr.port()), "").unwrap();
    let identity = Identity::new(2);
    let link = identity.link(address.clone(), Transport::Auto);
    let with_ca = |transport| {
        RelayClient::new(
            address.clone(),
            ClientConfig {
                extra_ca_pem: Some(tls.ca.clone()),
                ..config(transport)
            },
        )
        .unwrap()
    };
    let _backend = Backend::start(with_ca(Transport::Auto), identity, quick_policy()).await;
    for transport in ALL {
        let opener = with_ca(transport);
        round_trip(&opener, &link, b"over tls").await;
    }
    let auto = with_ca(Transport::Auto).binding().await.unwrap();
    assert_eq!(auto.binding, Binding::Grpc);
    relay.stop().await;
}

// (c) An HTTP/1.1-only hop that forwards WebSocket upgrades but not HTTP/2:
// auto classifies gRPC as broken and lands on WebSocket.
#[tokio::test]
async fn c_http1_only_hop_makes_auto_use_websocket() {
    let relay = Relay::start(support::relay_config()).await;
    let hop = HttpHop::start(
        relay.addr,
        HttpHopOptions {
            http1_only: true,
            ..HttpHopOptions::default()
        },
    )
    .await;
    let address = plain(hop.addr, "");
    let chosen = splice_through(&address, &[Transport::Auto, Transport::Ws]).await;
    assert_eq!(chosen, [Binding::Ws, Binding::Ws]);
    let choice = client(&address, Transport::Auto).binding().await.unwrap();
    match choice.reason {
        ChoiceReason::GrpcFailed(failure) => {
            assert_eq!(failure.kind, ProbeFailureKind::NoHttp2, "{failure}");
        }
        other => panic!("expected a gRPC failure, got {other:?}"),
    }
    // Pinned gRPC fails instead of silently falling back.
    let pinned = client(&address, Transport::Grpc);
    let link = Identity::new(9).link(address.clone(), Transport::Grpc);
    assert!(pinned.open(link.room_id()).await.is_err());
    relay.stop().await;
}

// (d) An h2 hop that strips trailers: the gRPC status never arrives, so the
// probe classifies gRPC as broken and auto uses WebSocket.
#[tokio::test]
async fn d_trailer_stripping_hop_makes_auto_use_websocket() {
    let relay = Relay::start(support::relay_config()).await;
    let hop = HttpHop::start(
        relay.addr,
        HttpHopOptions {
            strip_trailers: true,
            ..HttpHopOptions::default()
        },
    )
    .await;
    let address = plain(hop.addr, "");
    let chosen = splice_through(&address, &[Transport::Auto, Transport::Ws]).await;
    assert_eq!(chosen, [Binding::Ws, Binding::Ws]);
    let choice = client(&address, Transport::Auto).binding().await.unwrap();
    match choice.reason {
        ChoiceReason::GrpcFailed(failure) => {
            assert_eq!(
                failure.kind,
                ProbeFailureKind::TrailersStripped,
                "{failure}"
            );
        }
        other => panic!("expected a gRPC failure, got {other:?}"),
    }
    relay.stop().await;
}

/// Idle cut of the (e) hop, well above the 50 ms test heartbeat.
const IDLE_CUT: Duration = Duration::from_millis(300);

// (e) A hop that cuts connections idle for N ms: heartbeats keep both the
// control stream and an idle data stream alive for several N.
#[tokio::test]
async fn e_heartbeats_keep_streams_alive_through_an_idle_cutting_hop() {
    let relay = Relay::start(support::relay_config()).await;
    let hop = TcpHop::start(
        relay.addr,
        TcpHopOptions {
            idle_cut: Some(IDLE_CUT),
            ..TcpHopOptions::default()
        },
    )
    .await;
    let address = plain(hop.addr, "");
    for transport in [Transport::Grpc, Transport::Ws] {
        let identity = Identity::new(3);
        let link = identity.link(address.clone(), transport);
        let backend = Backend::start(client(&address, transport), identity, quick_policy()).await;
        let opener = client(&address, transport);
        let mut tunnel = support::connect(&opener, &link).await.unwrap();
        ping(&mut tunnel, b"before").await.unwrap();
        sleep(IDLE_CUT * 4).await;
        timeout(WAIT, ping(&mut tunnel, b"after the idle period"))
            .await
            .expect("in time")
            .unwrap_or_else(|e| panic!("{transport}: the idle stream was cut: {e}"));
        // The control stream survived too: no re-registration was needed.
        assert_eq!(
            backend.registrations.load(Ordering::SeqCst),
            1,
            "{transport}"
        );
        round_trip(&opener, &link, b"new stream").await;
    }
    relay.stop().await;
}

// (e') Control: without heartbeats the same hop does cut an idle stream.
#[tokio::test]
async fn e_without_heartbeats_the_idle_cut_happens() {
    let relay = Relay::start(support::relay_config()).await;
    let hop = TcpHop::start(
        relay.addr,
        TcpHopOptions {
            idle_cut: Some(IDLE_CUT),
            ..TcpHopOptions::default()
        },
    )
    .await;
    let identity = Identity::new(4);
    // The backend talks to the relay directly; only the opener crosses the hop.
    let _backend = Backend::start(
        client(&relay.address(), Transport::Ws),
        identity_clone(&identity),
        quick_policy(),
    )
    .await;
    let address = plain(hop.addr, "");
    let link = identity.link(address.clone(), Transport::Ws);
    let silent = RelayClient::new(
        address,
        ClientConfig {
            heartbeat: HeartbeatConfig::disabled(),
            ..config(Transport::Ws)
        },
    )
    .unwrap();
    let mut tunnel = support::connect(&silent, &link).await.unwrap();
    ping(&mut tunnel, b"before").await.unwrap();
    sleep(IDLE_CUT * 4).await;
    let after = timeout(WAIT, ping(&mut tunnel, b"after"))
        .await
        .expect("in time");
    assert!(after.is_err(), "the hop did not cut the idle stream");
    assert!(hop.cuts.load(Ordering::SeqCst) >= 1);
    relay.stop().await;
}

fn identity_clone(identity: &Identity) -> Identity {
    Identity {
        key: identity.key.clone(),
        noise: hya_relay::keys::StaticKeypair::from_secret(*identity.noise.secret()).unwrap(),
        psk: identity.psk.clone(),
    }
}

/// Lifetime cap of the (f) hop.
const MAX_DURATION: Duration = Duration::from_millis(400);

// (f) A hop that cuts every connection after N ms: the reconnect policy
// re-registers the host, and new opens succeed after each cut.
#[tokio::test]
async fn f_the_host_re_registers_after_max_duration_cuts() {
    let relay = Relay::start(support::relay_config()).await;
    let hop = TcpHop::start(
        relay.addr,
        TcpHopOptions {
            max_duration: Some(MAX_DURATION),
            ..TcpHopOptions::default()
        },
    )
    .await;
    let address = plain(hop.addr, "");
    for transport in [Transport::Grpc, Transport::Ws] {
        let identity = Identity::new(5);
        let link = identity.link(address.clone(), transport);
        let backend = Backend::start(client(&address, transport), identity, quick_policy()).await;
        for cut in 2..=3 {
            backend.wait_registrations(cut).await;
            // The first attempts may race the cut; a fresh stream succeeds.
            let opened = timeout(WAIT, async {
                loop {
                    let opener = client(&address, transport);
                    match support::connect(&opener, &link).await {
                        Ok(mut tunnel) => {
                            if ping(&mut tunnel, b"after the cut").await.is_ok() {
                                return;
                            }
                        }
                        Err(ClientError::RoomOffline(_) | ClientError::Unavailable(_)) => {}
                        Err(_) => {}
                    }
                    sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            assert!(opened.is_ok(), "{transport}: no stream after cut {cut}");
        }
        assert!(hop.cuts.load(Ordering::SeqCst) >= 2);
    }
    relay.stop().await;
}

// (g) A hop that routes only a path prefix to the relay (running with the
// same --path-prefix): both bindings work under the prefix.
#[tokio::test]
async fn g_prefix_routing_hop_carries_both_bindings() {
    let relay = Relay::start(support::relay_config().path_prefix("/relay").unwrap()).await;
    let hop = HttpHop::start(
        relay.addr,
        HttpHopOptions {
            route_prefix: Some("/relay".to_owned()),
            ..HttpHopOptions::default()
        },
    )
    .await;
    let address = plain(hop.addr, "/relay");
    let chosen = splice_through(&address, &ALL).await;
    assert_eq!(chosen, [Binding::Grpc, Binding::Grpc, Binding::Ws]);
    // Without the prefix the hop answers 404 on both bindings.
    let wrong = client(&plain(hop.addr, ""), Transport::Auto);
    match wrong.binding().await.unwrap_err() {
        ClientError::NoBinding { grpc, ws } => {
            assert_eq!(grpc.kind, ProbeFailureKind::HopRejected, "{grpc}");
            assert_eq!(ws.kind, ProbeFailureKind::HopRejected, "{ws}");
        }
        other => panic!("expected NoBinding, got {other:?}"),
    }
    relay.stop().await;
}

// (h) A hop that rewrites Host / :authority: nothing depends on it.
#[tokio::test]
async fn h_host_rewriting_hop_carries_both_bindings() {
    let relay = Relay::start(support::relay_config()).await;
    let hop = HttpHop::start(
        relay.addr,
        HttpHopOptions {
            rewrite_host: Some("relay.internal:1".to_owned()),
            ..HttpHopOptions::default()
        },
    )
    .await;
    let address = plain(hop.addr, "");
    let chosen = splice_through(&address, &ALL).await;
    assert_eq!(chosen, [Binding::Grpc, Binding::Grpc, Binding::Ws]);
    relay.stop().await;
}

// (i) Plaintext `hya+insecure://` straight to the relay (the tailnet shape),
// from a parsed link.
#[tokio::test]
async fn i_insecure_link_direct_carries_both_bindings() {
    let relay = Relay::start(support::relay_config()).await;
    let identity = Identity::new(6);
    let base = identity.link(relay.address(), Transport::Auto);
    let text = base.to_secret_string();
    assert!(text.starts_with("hya+insecure://127.0.0.1:"), "{text}");
    let _backend = Backend::start(
        client(&relay.address(), Transport::Auto),
        identity,
        quick_policy(),
    )
    .await;
    for t in ["auto", "grpc", "ws"] {
        let text = if t == "auto" {
            text.clone()
        } else {
            text.replacen('#', &format!("?t={t}#"), 1)
        };
        let link = RelayLink::parse(&text).unwrap();
        let opener = RelayClient::from_link(&link, config(Transport::Auto)).unwrap();
        round_trip(&opener, &link, b"over the tailnet").await;
        let expected = if t == "ws" {
            Binding::Ws
        } else {
            Binding::Grpc
        };
        assert_eq!(opener.binding().await.unwrap().binding, expected, "t={t}");
    }
    relay.stop().await;
}
