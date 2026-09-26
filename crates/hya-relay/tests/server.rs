#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]
//! The server-side bindings over real sockets: gRPC and WebSocket on one
//! port, cross-binding splices, error mapping, path prefix, TLS, forwarded
//! client addresses, per-client limits, the plain-HTTP fallback, and
//! graceful shutdown.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use futures::{Sink, SinkExt, Stream, StreamExt};
use hya_relay::link::RoomId;
use hya_relay::proto::relay_client::RelayClient;
use hya_relay::proto::{
    Accept, Chunk, Close, HostFrame, Open, Opened, ProxyToHost, Register, RelayErrorCode, chunk,
    host_frame, proxy_to_host, register_signing_message,
};
use hya_relay::proxy::ProxyLimits;
use hya_relay::server::{RelayServer, RelayServerConfig, TlsFiles, WS_CLOSE_ERROR_BASE};
use hyper_util::rt::TokioIo;
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tonic::codegen::http;
use tonic::transport::Endpoint;
use tower::util::BoxCloneService;
use tower::{ServiceBuilder, ServiceExt};

const WAIT: Duration = Duration::from_secs(10);

// ---- server ----

struct Running {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: JoinHandle<()>,
}

impl Running {
    async fn shutdown(mut self) {
        let _ = self.stop.take().unwrap().send(());
        timeout(WAIT, self.done)
            .await
            .expect("server stopped in time")
            .unwrap();
    }
}

fn config() -> RelayServerConfig {
    RelayServerConfig::new("127.0.0.1:0".parse().unwrap())
}

async fn start(config: RelayServerConfig) -> Running {
    let (stop, stopped) = oneshot::channel::<()>();
    let (addr, serve) = RelayServer::bind(config, async move {
        let _ = stopped.await;
    })
    .await
    .unwrap();
    Running {
        addr,
        stop: Some(stop),
        done: tokio::spawn(serve),
    }
}

// ---- keys ----

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn room_of(key: &SigningKey) -> String {
    RoomId::from_ed25519(key.verifying_key().as_bytes())
        .as_str()
        .to_owned()
}

// ---- TLS ----

struct Tls {
    files: TlsFiles,
    connector: TlsConnector,
    ws_connector: TlsConnector,
}

fn tls() -> Tls {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "hya-relay-server-tls-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cert: PathBuf = dir.join("cert.pem");
    let key: PathBuf = dir.join("key.pem");
    std::fs::write(&cert, certified.cert.pem()).unwrap();
    std::fs::write(&key, certified.key_pair.serialize_pem()).unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certified.cert.der().to_vec()))
        .unwrap();
    let client_config = |alpn: &[u8]| {
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots.clone())
        .with_no_client_auth();
        config.alpn_protocols = vec![alpn.to_vec()];
        TlsConnector::from(Arc::new(config))
    };
    Tls {
        files: TlsFiles { cert, key },
        connector: client_config(b"h2"),
        ws_connector: client_config(b"http/1.1"),
    }
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// ---- a binding-neutral test leg ----

#[derive(Debug)]
enum Item<Rx> {
    Msg(Rx),
    /// gRPC stream ended with a non-OK status.
    Status(tonic::Code, String),
    /// WebSocket close frame (code).
    WsClose(Option<u16>),
    /// Stream ended (gRPC OK trailers, or WebSocket EOF).
    End,
}

type BoxSink<Tx> = Pin<Box<dyn Sink<Tx, Error = String> + Send>>;
type BoxStream<Rx> = Pin<Box<dyn Stream<Item = Item<Rx>> + Send>>;

struct Leg<Tx, Rx> {
    sink: BoxSink<Tx>,
    stream: BoxStream<Rx>,
}

impl<Tx, Rx: std::fmt::Debug> Leg<Tx, Rx> {
    async fn send(&mut self, message: Tx) {
        timeout(WAIT, self.sink.send(message))
            .await
            .expect("send in time")
            .expect("send ok");
    }

    async fn next(&mut self) -> Item<Rx> {
        timeout(WAIT, self.stream.next())
            .await
            .expect("item in time")
            .unwrap_or(Item::End)
    }

    async fn recv(&mut self) -> Rx {
        match self.next().await {
            Item::Msg(message) => message,
            other => panic!("expected a message, got {other:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Binding {
    Grpc,
    Ws,
}

/// Where and how a test client connects.
#[derive(Clone)]
struct Target {
    addr: SocketAddr,
    prefix: String,
    headers: Vec<(&'static str, String)>,
    tls: Option<(TlsConnector, TlsConnector)>,
}

impl Target {
    fn new(server: &Running) -> Self {
        Self {
            addr: server.addr,
            prefix: String::new(),
            headers: Vec::new(),
            tls: None,
        }
    }

    fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = prefix.to_owned();
        self
    }

    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }

    fn tls(mut self, tls: &Tls) -> Self {
        self.tls = Some((tls.connector.clone(), tls.ws_connector.clone()));
        self
    }
}

type GrpcService = BoxCloneService<
    http::Request<tonic::body::Body>,
    http::Response<tonic::body::Body>,
    tonic::transport::Error,
>;

async fn grpc_client(target: &Target) -> RelayClient<GrpcService> {
    let endpoint = Endpoint::from_shared(format!("http://{}", target.addr)).unwrap();
    let channel = match &target.tls {
        None => endpoint.connect().await.unwrap(),
        Some((connector, _)) => {
            let connector = connector.clone();
            let addr = target.addr;
            endpoint
                .connect_with_connector(tower::service_fn(move |_: http::Uri| {
                    let connector = connector.clone();
                    async move {
                        let tcp = TcpStream::connect(addr).await?;
                        let name = ServerName::try_from("localhost").unwrap();
                        let tls = connector.connect(name, tcp).await?;
                        Ok::<_, std::io::Error>(TokioIo::new(tls))
                    }
                }))
                .await
                .unwrap()
        }
    };
    let prefix = target.prefix.clone();
    let service = ServiceBuilder::new()
        .map_request(move |mut request: http::Request<tonic::body::Body>| {
            if !prefix.is_empty() {
                let path = format!("{prefix}{}", request.uri().path());
                let mut parts = request.uri().clone().into_parts();
                parts.path_and_query = Some(path.parse().unwrap());
                *request.uri_mut() = http::Uri::from_parts(parts).unwrap();
            }
            request
        })
        .service(channel);
    RelayClient::new(BoxCloneService::new(service.boxed_clone()))
}

fn grpc_request<T>(
    target: &Target,
    stream: ReceiverStream<T>,
) -> tonic::Request<ReceiverStream<T>> {
    let mut request = tonic::Request::new(stream);
    for (name, value) in &target.headers {
        request.metadata_mut().insert(*name, value.parse().unwrap());
    }
    request
}

fn grpc_leg<Tx: Send + 'static, Rx: Send + 'static>(
    tx: mpsc::Sender<Tx>,
    response: tonic::Streaming<Rx>,
) -> Leg<Tx, Rx> {
    let sink = tokio_util::sync::PollSender::new(tx).sink_map_err(|e| e.to_string());
    let stream = response
        .map(|item| match item {
            Ok(message) => Item::Msg(message),
            Err(status) => Item::Status(status.code(), status.message().to_owned()),
        })
        .chain(futures::stream::once(async { Item::End }));
    Leg {
        sink: Box::pin(sink),
        stream: Box::pin(stream),
    }
}

async fn grpc_host(target: &Target) -> Result<Leg<HostFrame, ProxyToHost>, tonic::Status> {
    let mut client = grpc_client(target).await;
    let (tx, rx) = mpsc::channel(16);
    let response = client
        .host(grpc_request(target, ReceiverStream::new(rx)))
        .await?;
    Ok(grpc_leg(tx, response.into_inner()))
}

async fn grpc_chunk(target: &Target, open: bool) -> Result<Leg<Chunk, Chunk>, tonic::Status> {
    let mut client = grpc_client(target).await;
    let (tx, rx) = mpsc::channel(16);
    let request = grpc_request(target, ReceiverStream::new(rx));
    let response = if open {
        client.open(request).await?
    } else {
        client.accept(request).await?
    };
    Ok(grpc_leg(tx, response.into_inner()))
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn connect_io(target: &Target, ws: bool) -> Box<dyn Io> {
    let tcp = TcpStream::connect(target.addr).await.unwrap();
    match &target.tls {
        None => Box::new(tcp),
        Some((grpc, websocket)) => {
            let connector = if ws { websocket } else { grpc };
            let name = ServerName::try_from("localhost").unwrap();
            Box::new(connector.connect(name, tcp).await.unwrap())
        }
    }
}

async fn ws_connect(
    target: &Target,
    route: &str,
) -> Result<tokio_tungstenite::WebSocketStream<Box<dyn Io>>, tokio_tungstenite::tungstenite::Error>
{
    let url = format!(
        "ws://localhost:{}{}/hya.relay.v1/ws/{route}",
        target.addr.port(),
        target.prefix
    );
    let mut request = url.into_client_request().unwrap();
    for (name, value) in &target.headers {
        request.headers_mut().insert(*name, value.parse().unwrap());
    }
    let io = connect_io(target, true).await;
    let (socket, _) = tokio_tungstenite::client_async(request, io).await?;
    Ok(socket)
}

fn ws_leg<Tx, Rx>(socket: tokio_tungstenite::WebSocketStream<Box<dyn Io>>) -> Leg<Tx, Rx>
where
    Tx: prost::Message + Send + 'static,
    Rx: prost::Message + Default + Send + 'static,
{
    let (write, read) = socket.split();
    let sink =
        write
            .sink_map_err(|e| e.to_string())
            .with(|message: Tx| async move {
                Ok::<_, String>(WsMessage::Binary(message.encode_to_vec()))
            });
    let stream = read
        .filter_map(|item| async move {
            match item {
                Ok(WsMessage::Binary(bytes)) => Some(Item::Msg(Rx::decode(&bytes[..]).unwrap())),
                Ok(WsMessage::Close(frame)) => {
                    Some(Item::WsClose(frame.map(|f| u16::from(f.code))))
                }
                Ok(WsMessage::Text(text)) => panic!("unexpected text frame {text:?}"),
                Ok(_) => None,
                Err(_) => Some(Item::End),
            }
        })
        .chain(futures::stream::once(async { Item::End }));
    Leg {
        sink: Box::pin(sink),
        stream: Box::pin(stream),
    }
}

async fn host_leg(target: &Target, binding: Binding) -> Leg<HostFrame, ProxyToHost> {
    match binding {
        Binding::Grpc => grpc_host(target).await.unwrap(),
        Binding::Ws => ws_leg(ws_connect(target, "host").await.unwrap()),
    }
}

async fn chunk_leg(target: &Target, binding: Binding, open: bool) -> Leg<Chunk, Chunk> {
    match binding {
        Binding::Grpc => grpc_chunk(target, open).await.unwrap(),
        Binding::Ws => ws_leg(
            ws_connect(target, if open { "open" } else { "accept" })
                .await
                .unwrap(),
        ),
    }
}

// ---- relay frames ----

fn register_frame(key: &SigningKey, nonce: &[u8]) -> HostFrame {
    HostFrame {
        frame: Some(host_frame::Frame::Register(Register {
            ed25519_pubkey: key.verifying_key().as_bytes().to_vec(),
            signature: key
                .sign(&register_signing_message(nonce))
                .to_bytes()
                .to_vec(),
        })),
    }
}

async fn challenge(host: &mut Leg<HostFrame, ProxyToHost>) -> Vec<u8> {
    match host.recv().await.frame {
        Some(proxy_to_host::Frame::Challenge(c)) => c.nonce,
        other => panic!("expected challenge, got {other:?}"),
    }
}

/// Register `key`; the next host item is returned (registered or error).
async fn try_register(
    host: &mut Leg<HostFrame, ProxyToHost>,
    key: &SigningKey,
) -> Item<ProxyToHost> {
    let nonce = challenge(host).await;
    host.send(register_frame(key, &nonce)).await;
    host.next().await
}

async fn register(
    target: &Target,
    binding: Binding,
    key: &SigningKey,
) -> Leg<HostFrame, ProxyToHost> {
    let mut host = host_leg(target, binding).await;
    match try_register(&mut host, key).await {
        Item::Msg(ProxyToHost {
            frame: Some(proxy_to_host::Frame::Registered(r)),
        }) => assert_eq!(r.room_id, room_of(key)),
        other => panic!("expected registered, got {other:?}"),
    }
    host
}

/// Expect a relay failure with `code` in the binding's form: a gRPC status,
/// or a final error frame then a close with `4000 + code`.
async fn expect_host_failure(
    host: &mut Leg<HostFrame, ProxyToHost>,
    binding: Binding,
    code: RelayErrorCode,
) {
    expect_failure(host, binding, code, |m: ProxyToHost| match m.frame {
        Some(proxy_to_host::Frame::Error(e)) => Some(e.error_code()),
        _ => None,
    })
    .await;
}

async fn expect_chunk_failure(leg: &mut Leg<Chunk, Chunk>, binding: Binding, code: RelayErrorCode) {
    expect_failure(leg, binding, code, |m: Chunk| match m.frame {
        Some(chunk::Frame::Error(e)) => Some(e.error_code()),
        _ => None,
    })
    .await;
}

async fn expect_failure<Tx, Rx: std::fmt::Debug>(
    leg: &mut Leg<Tx, Rx>,
    binding: Binding,
    code: RelayErrorCode,
    error_of: impl Fn(Rx) -> Option<RelayErrorCode>,
) {
    match binding {
        Binding::Grpc => match leg.next().await {
            Item::Status(status, _) => {
                assert_eq!(status, hya_relay::proto::relay_error_code_to_grpc(code));
            }
            other => panic!("expected status {code:?}, got {other:?}"),
        },
        Binding::Ws => {
            match leg.next().await {
                Item::Msg(message) => assert_eq!(error_of(message), Some(code)),
                other => panic!("expected error frame {code:?}, got {other:?}"),
            }
            match leg.next().await {
                Item::WsClose(close) => {
                    assert_eq!(close, Some(WS_CLOSE_ERROR_BASE + code as u16));
                }
                other => panic!("expected close, got {other:?}"),
            }
        }
    }
}

fn data(bytes: &[u8]) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Data(bytes.to_vec())),
    }
}

fn close() -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Close(Close {})),
    }
}

fn open(room: &str) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Open(Open {
            room_id: room.to_owned(),
        })),
    }
}

fn accept(stream_id: &str) -> Chunk {
    Chunk {
        frame: Some(chunk::Frame::Accept(Accept {
            stream_id: stream_id.to_owned(),
        })),
    }
}

async fn incoming(host: &mut Leg<HostFrame, ProxyToHost>) -> String {
    match host.recv().await.frame {
        Some(proxy_to_host::Frame::Incoming(i)) => i.stream_id,
        other => panic!("expected incoming, got {other:?}"),
    }
}

/// Whether a stream ended cleanly in the binding's form.
async fn expect_clean_end<Tx, Rx: std::fmt::Debug>(leg: &mut Leg<Tx, Rx>, binding: Binding) {
    match (binding, leg.next().await) {
        (Binding::Grpc, Item::End) => {}
        (Binding::Ws, Item::WsClose(Some(1000))) => {}
        (_, other) => panic!("expected a clean end over {binding:?}, got {other:?}"),
    }
}

/// Register a host over `host_binding`, open over `open_binding`, accept
/// over `host_binding`, and run a full round trip with half-closes.
async fn round_trip(target: &Target, host_binding: Binding, open_binding: Binding) {
    let k = key(1);
    let mut host = register(target, host_binding, &k).await;
    let mut opener = chunk_leg(target, open_binding, true).await;
    opener.send(open(&room_of(&k))).await;
    let id = incoming(&mut host).await;
    let mut accepted = chunk_leg(target, host_binding, false).await;
    accepted.send(accept(&id)).await;
    assert_eq!(
        opener.recv().await,
        Chunk {
            frame: Some(chunk::Frame::Opened(Opened {}))
        }
    );

    opener.send(data(b"ping")).await;
    assert_eq!(accepted.recv().await, data(b"ping"));
    accepted.send(data(b"pong")).await;
    assert_eq!(opener.recv().await, data(b"pong"));

    // Half-close: the opener closes first, the host still answers.
    opener.send(close()).await;
    assert_eq!(accepted.recv().await, close());
    accepted.send(data(b"late")).await;
    assert_eq!(opener.recv().await, data(b"late"));
    accepted.send(close()).await;
    assert_eq!(opener.recv().await, close());
    expect_clean_end(&mut opener, open_binding).await;
    expect_clean_end(&mut accepted, host_binding).await;
}

// ---- tests ----

#[tokio::test]
async fn grpc_host_and_websocket_opener_splice() {
    let server = start(config()).await;
    round_trip(&Target::new(&server), Binding::Grpc, Binding::Ws).await;
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_host_and_grpc_opener_splice() {
    let server = start(config()).await;
    round_trip(&Target::new(&server), Binding::Ws, Binding::Grpc).await;
    server.shutdown().await;
}

#[tokio::test]
async fn same_binding_splices_work() {
    let server = start(config()).await;
    round_trip(&Target::new(&server), Binding::Grpc, Binding::Grpc).await;
    round_trip(&Target::new(&server), Binding::Ws, Binding::Ws).await;
    server.shutdown().await;
}

#[tokio::test]
async fn open_to_an_offline_room_is_not_found_on_both_bindings() {
    let server = start(config()).await;
    let target = Target::new(&server);
    for binding in [Binding::Grpc, Binding::Ws] {
        let mut opener = chunk_leg(&target, binding, true).await;
        opener.send(open(&room_of(&key(9)))).await;
        expect_chunk_failure(&mut opener, binding, RelayErrorCode::NotFound).await;
    }
    server.shutdown().await;
}

#[tokio::test]
async fn grpc_error_is_the_status_not_a_message() {
    let server = start(config()).await;
    let target = Target::new(&server);
    let mut opener = chunk_leg(&target, Binding::Grpc, true).await;
    opener.send(open(&room_of(&key(9)))).await;
    match opener.next().await {
        Item::Status(tonic::Code::NotFound, message) => assert_eq!(message, "room is offline"),
        other => panic!("expected NOT_FOUND status, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_text_frames_are_rejected() {
    let server = start(config()).await;
    let target = Target::new(&server);
    let mut socket = ws_connect(&target, "open").await.unwrap();
    socket
        .send(WsMessage::Text("open please".into()))
        .await
        .unwrap();
    let close = timeout(WAIT, async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Close(frame))) => break frame.map(|f| u16::from(f.code)),
                Some(Ok(_)) => {}
                other => panic!("expected close, got {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(close, Some(1003));
    server.shutdown().await;
}

#[tokio::test]
async fn path_prefix_applies_to_both_bindings() {
    let server = start(config().path_prefix("/relay/hya/").unwrap()).await;
    let prefixed = Target::new(&server).prefix("/relay/hya");
    round_trip(&prefixed, Binding::Grpc, Binding::Ws).await;
    round_trip(&prefixed, Binding::Ws, Binding::Grpc).await;

    // Without the prefix neither binding answers.
    let bare = Target::new(&server);
    let status = match grpc_host(&bare).await {
        Ok(mut host) => match host.next().await {
            Item::Status(code, _) => code,
            other => panic!("expected a status, got {other:?}"),
        },
        Err(status) => status.code(),
    };
    assert_eq!(status, tonic::Code::Unimplemented);
    match ws_connect(&bare, "host").await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 404);
        }
        other => panic!("expected HTTP 404, got {:?}", other.map(|_| ())),
    }
    server.shutdown().await;
}

#[test]
fn invalid_path_prefixes_are_rejected() {
    for bad in ["/a b", "/../x", "/./x", "/a//b", "/a?b", "/é"] {
        assert!(config().path_prefix(bad).is_err(), "{bad:?} accepted");
    }
    for good in ["", "/", "hya", "/hya/", "/a/b-c_d.e~f"] {
        assert!(config().path_prefix(good).is_ok(), "{good:?} rejected");
    }
}

async fn http_get(target: &Target, path: &str) -> String {
    let mut io = connect_io(target, true).await;
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    io.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    timeout(WAIT, io.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(response).unwrap()
}

#[tokio::test]
async fn other_paths_are_404_hya_relay() {
    let server = start(config().path_prefix("/p").unwrap()).await;
    let target = Target::new(&server);
    for path in [
        "/",
        "/p",
        "/p/hya.relay.v1/ws/nope",
        "/hya.relay.v1/ws/host",
        "/favicon.ico",
    ] {
        let response = http_get(&target, path).await;
        assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
        assert!(
            response.ends_with("\r\n\r\nhya relay"),
            "{path}: {response}"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn tls_serves_both_bindings() {
    let tls = tls();
    let server = start(config().tls(tls.files.clone())).await;
    let target = Target::new(&server).tls(&tls);
    round_trip(&target, Binding::Grpc, Binding::Ws).await;
    round_trip(&target, Binding::Ws, Binding::Grpc).await;
    let response = http_get(&target, "/").await;
    assert!(response.ends_with("hya relay"), "{response}");
    server.shutdown().await;
}

#[tokio::test]
async fn missing_tls_files_fail_bind() {
    let result = RelayServer::bind(
        config().tls(TlsFiles {
            cert: "/nonexistent/cert.pem".into(),
            key: "/nonexistent/key.pem".into(),
        }),
        std::future::pending(),
    )
    .await;
    assert!(result.is_err());
}

fn one_room_per_client() -> ProxyLimits {
    ProxyLimits {
        max_rooms_per_peer: 1,
        ..ProxyLimits::default()
    }
}

async fn register_outcome(
    target: &Target,
    binding: Binding,
    key: &SigningKey,
) -> Result<Leg<HostFrame, ProxyToHost>, RelayErrorCode> {
    let mut host = host_leg(target, binding).await;
    match try_register(&mut host, key).await {
        Item::Msg(ProxyToHost {
            frame: Some(proxy_to_host::Frame::Registered(_)),
        }) => Ok(host),
        Item::Msg(ProxyToHost {
            frame: Some(proxy_to_host::Frame::Error(e)),
        }) => Err(e.error_code()),
        Item::Status(code, _) => Err(hya_relay::proto::relay_error_code_from_grpc(code)),
        other => panic!("unexpected registration outcome {other:?}"),
    }
}

#[tokio::test]
async fn trusted_forwarding_headers_identify_clients() {
    let server = start(config().trust_forwarded(true).limits(one_room_per_client())).await;
    let base = Target::new(&server);
    let a = base
        .clone()
        .header("x-forwarded-for", "203.0.113.1, 10.0.0.1");
    let b = base.clone().header("cf-connecting-ip", "203.0.113.2");
    let _a = register_outcome(&a, Binding::Grpc, &key(1)).await.unwrap();
    let _b = register_outcome(&b, Binding::Ws, &key(2)).await.unwrap();
    // CF-Connecting-IP wins over X-Real-IP and X-Forwarded-For.
    let c = base
        .clone()
        .header("cf-connecting-ip", "203.0.113.3")
        .header("x-real-ip", "203.0.113.1")
        .header("x-forwarded-for", "203.0.113.1");
    let _c = register_outcome(&c, Binding::Ws, &key(3)).await.unwrap();
    // X-Real-IP wins over X-Forwarded-For.
    let d = base
        .clone()
        .header("x-real-ip", "203.0.113.4")
        .header("x-forwarded-for", "203.0.113.1");
    let _d = register_outcome(&d, Binding::Grpc, &key(4)).await.unwrap();
    // The leftmost X-Forwarded-For entry is the client: 203.0.113.1 again.
    let e = base.clone().header("x-forwarded-for", "203.0.113.1");
    assert_eq!(
        register_outcome(&e, Binding::Ws, &key(5)).await.err(),
        Some(RelayErrorCode::ResourceExhausted)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn forwarding_headers_are_ignored_unless_trusted() {
    let server = start(config().limits(one_room_per_client())).await;
    let base = Target::new(&server);
    let a = base.clone().header("x-forwarded-for", "203.0.113.1");
    let b = base.clone().header("cf-connecting-ip", "203.0.113.2");
    let _a = register_outcome(&a, Binding::Grpc, &key(1)).await.unwrap();
    // Both come from 127.0.0.1.
    assert_eq!(
        register_outcome(&b, Binding::Ws, &key(2)).await.err(),
        Some(RelayErrorCode::ResourceExhausted)
    );
    assert_eq!(
        register_outcome(&b, Binding::Grpc, &key(3)).await.err(),
        Some(RelayErrorCode::ResourceExhausted)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn pending_registrations_per_client_are_limited_across_bindings() {
    let server = start(config().limits(ProxyLimits {
        max_pending_registrations_per_peer: 1,
        ..ProxyLimits::default()
    }))
    .await;
    let target = Target::new(&server);
    let mut first = host_leg(&target, Binding::Grpc).await;
    let nonce = challenge(&mut first).await;
    for binding in [Binding::Ws, Binding::Grpc] {
        let mut refused = host_leg(&target, binding).await;
        expect_host_failure(&mut refused, binding, RelayErrorCode::ResourceExhausted).await;
    }
    // Registering frees the slot.
    first.send(register_frame(&key(1), &nonce)).await;
    assert!(matches!(
        first.recv().await.frame,
        Some(proxy_to_host::Frame::Registered(_))
    ));
    let _second = register(&target, Binding::Ws, &key(2)).await;
    server.shutdown().await;
}

#[tokio::test]
async fn graceful_shutdown_ends_streams_with_unavailable() {
    let server = start(config()).await;
    let target = Target::new(&server);
    let k = key(1);
    let mut grpc_host = register(&target, Binding::Grpc, &k).await;
    let mut ws_host = register(&target, Binding::Ws, &key(2)).await;
    let mut opener = chunk_leg(&target, Binding::Ws, true).await;
    opener.send(open(&room_of(&k))).await;
    let id = incoming(&mut grpc_host).await;
    let mut accepted = chunk_leg(&target, Binding::Grpc, false).await;
    accepted.send(accept(&id)).await;
    assert!(matches!(
        opener.recv().await.frame,
        Some(chunk::Frame::Opened(_))
    ));

    let addr = server.addr;
    let (stop, done) = (server.stop, server.done);
    let _ = stop.unwrap().send(());
    expect_host_failure(&mut grpc_host, Binding::Grpc, RelayErrorCode::Unavailable).await;
    expect_host_failure(&mut ws_host, Binding::Ws, RelayErrorCode::Unavailable).await;
    expect_chunk_failure(&mut accepted, Binding::Grpc, RelayErrorCode::Unavailable).await;
    expect_chunk_failure(&mut opener, Binding::Ws, RelayErrorCode::Unavailable).await;
    timeout(WAIT, done).await.expect("serve ended").unwrap();
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "listener still open"
    );
}

#[tokio::test]
async fn shutdown_drains_idle_connections_promptly() {
    let server = start(config().drain_timeout(Duration::from_millis(500))).await;
    let target = Target::new(&server);
    // An idle HTTP/2 connection and a registered host.
    let _client = grpc_client(&target).await;
    let _host = register(&target, Binding::Ws, &key(1)).await;
    let started = tokio::time::Instant::now();
    server.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_dropped_client_ends_the_other_leg_with_unavailable() {
    let server = start(config()).await;
    let target = Target::new(&server);
    for (host_binding, open_binding) in [(Binding::Grpc, Binding::Ws), (Binding::Ws, Binding::Grpc)]
    {
        let k = key(1);
        let mut host = register(&target, host_binding, &k).await;
        let (mut opener, cut) = match open_binding {
            Binding::Grpc => {
                let (leg, tcp) = grpc_open_cuttable(&target).await;
                (leg, Some(tcp))
            }
            Binding::Ws => (chunk_leg(&target, open_binding, true).await, None),
        };
        opener.send(open(&room_of(&k))).await;
        let id = incoming(&mut host).await;
        let mut accepted = chunk_leg(&target, host_binding, false).await;
        accepted.send(accept(&id)).await;
        assert!(matches!(
            opener.recv().await.frame,
            Some(chunk::Frame::Opened(_))
        ));
        // The opener's connection is cut without `close`.
        if let Some(tcp) = cut {
            tcp.shutdown(std::net::Shutdown::Both).unwrap();
        }
        drop(opener);
        expect_chunk_failure(&mut accepted, host_binding, RelayErrorCode::Unavailable).await;
    }
    server.shutdown().await;
}

/// A gRPC `Open` leg whose TCP connection the test can cut.
async fn grpc_open_cuttable(target: &Target) -> (Leg<Chunk, Chunk>, std::net::TcpStream) {
    let tcp = std::net::TcpStream::connect(target.addr).unwrap();
    tcp.set_nonblocking(true).unwrap();
    let handle = tcp.try_clone().unwrap();
    let slot = Arc::new(std::sync::Mutex::new(Some(tcp)));
    let channel = Endpoint::from_shared(format!("http://{}", target.addr))
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: http::Uri| {
            let slot = slot.clone();
            async move {
                let tcp = slot
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| std::io::Error::other("connection already used"))?;
                Ok::<_, std::io::Error>(TokioIo::new(TcpStream::from_std(tcp)?))
            }
        }))
        .await
        .unwrap();
    let mut client = RelayClient::new(channel);
    let (tx, rx) = mpsc::channel(16);
    let response = client.open(ReceiverStream::new(rx)).await.unwrap();
    (grpc_leg(tx, response.into_inner()), handle)
}
