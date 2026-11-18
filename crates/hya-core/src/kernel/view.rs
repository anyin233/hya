use hya_proto::{EventSeq, Projection};

/// The session log prefix a model round reads.
pub struct View<'a> {
    /// The folded projection of that prefix.
    pub projection: &'a Projection,
}

impl View<'_> {
    /// Sequence of the last durable event included in the prefix.
    #[must_use]
    pub fn cut(&self) -> EventSeq {
        EventSeq(self.projection.last_seq)
    }
}
