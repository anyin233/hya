//! Copy work performed by the real reducer/cache during lifecycle benchmarks.
use std::sync::atomic::{AtomicU64, Ordering};

static PROJECTION_CLONES: AtomicU64 = AtomicU64::new(0);
static PROJECTION_CLONE_BYTES: AtomicU64 = AtomicU64::new(0);
static MAIL_CLONE_BYTES: AtomicU64 = AtomicU64::new(0);

/// Cumulative (projection clones, logical projection bytes copied, mail body bytes copied).
/// Projection bytes use JSON encoded length as a stable payload-size proxy, not heap size.
pub fn snapshot() -> (u64, u64, u64) {
    (
        PROJECTION_CLONES.load(Ordering::Relaxed),
        PROJECTION_CLONE_BYTES.load(Ordering::Relaxed),
        MAIL_CLONE_BYTES.load(Ordering::Relaxed),
    )
}

pub(crate) fn projection_clone(projection: &crate::Projection) {
    struct Bytes(u64);
    impl std::io::Write for Bytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Bytes(0);
    // Projection serialization is infallible and the counting writer never fails.
    if let Err(error) = serde_json::to_writer(&mut bytes, projection) {
        panic!("benchmark projection encoding: {error}");
    }
    PROJECTION_CLONES.fetch_add(1, Ordering::Relaxed);
    PROJECTION_CLONE_BYTES.fetch_add(bytes.0, Ordering::Relaxed);
}

pub(crate) fn mail_clone(bytes: u64) {
    MAIL_CLONE_BYTES.fetch_add(bytes, Ordering::Relaxed);
}
