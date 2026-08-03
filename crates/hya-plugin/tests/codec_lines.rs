#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use hya_plugin::PluginError;
use hya_plugin::codec::{FrameReader, MAX_LINE_BYTES};
use hya_plugin::protocol::Frame;
use tokio::io::{AsyncWriteExt, duplex};
use tokio::sync::{Barrier, oneshot};

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
async fn oversized_open_line_is_rejected_before_writer_eof() {
    let (mut writer, reader_io) = duplex(8 * 1024);
    let payload = vec![b'x'; MAX_LINE_BYTES + 1];
    let barrier = Arc::new(Barrier::new(2));
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let writer_barrier = Arc::clone(&barrier);
    let writer_task = tokio::spawn(async move {
        writer_barrier.wait().await;
        writer.write_all(&payload).await.unwrap();
        let _ = release_rx.await;
    });

    let mut reader = FrameReader::new(reader_io);
    barrier.wait().await;
    let result = tokio::time::timeout(Duration::from_millis(250), reader.next()).await;

    drop(release_tx);
    writer_task.abort();
    let _ = writer_task.await;
    assert!(matches!(
        result,
        Ok(Err(PluginError::OversizedLine(MAX_LINE_BYTES)))
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
