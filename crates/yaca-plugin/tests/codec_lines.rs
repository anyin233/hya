#![allow(clippy::unwrap_used, clippy::expect_used)]

use yaca_plugin::PluginError;
use yaca_plugin::codec::{FrameReader, MAX_LINE_BYTES};
use yaca_plugin::protocol::Frame;

#[tokio::test]
async fn reads_valid_frames_then_eofs() {
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{}}\n",
    );
    let mut reader = FrameReader::new(input.as_bytes());
    assert!(matches!(
        reader.next().await.unwrap(),
        Some(Frame::Request(_))
    ));
    assert!(matches!(
        reader.next().await.unwrap(),
        Some(Frame::Notification(_))
    ));
    assert!(reader.next().await.unwrap().is_none());
}

#[tokio::test]
async fn malformed_line_errors() {
    let mut reader = FrameReader::new(&b"not json at all\n"[..]);
    assert!(matches!(reader.next().await, Err(PluginError::Json(_))));
}

#[tokio::test]
async fn oversized_line_errors() {
    let mut big = vec![b'x'; MAX_LINE_BYTES + 8];
    big.push(b'\n');
    let mut reader = FrameReader::new(&big[..]);
    assert!(matches!(
        reader.next().await,
        Err(PluginError::OversizedLine(_))
    ));
}

#[tokio::test]
async fn long_line_split_across_reads_is_not_corrupted() {
    let mut payload =
        String::from("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"x\",\"params\":{\"pad\":\"");
    payload.push_str(&"a".repeat(200_000));
    payload.push_str("\"}}\n");
    let mut reader = FrameReader::new(payload.as_bytes());
    match reader.next().await.unwrap().unwrap() {
        Frame::Request(req) => assert_eq!(req.method, "x"),
        other => panic!("expected request, got {other:?}"),
    }
    assert!(reader.next().await.unwrap().is_none());
}
