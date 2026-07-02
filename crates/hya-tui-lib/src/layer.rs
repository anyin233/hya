//! Layer-aware paint claims and overlap validation.

use crate::contracts::{LayoutResult, NodeId, Rect};

/// Identifies a paint layer in ascending z-order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LayerId(
    /// Numeric z-order where lower layers paint before higher layers.
    pub u16,
);

impl LayerId {
    /// Returns the default base paint layer.
    #[must_use]
    pub const fn base() -> Self {
        Self(0)
    }
}

/// Associates a solved rectangle with the component id and layer that will paint it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayeredRect {
    /// Identifier of the node that owns this paint claim.
    pub id: NodeId,
    /// Layer that this node paints on.
    pub layer: LayerId,
    /// Visible rectangle claimed by the node.
    pub rect: Rect,
}

/// Reports invalid or missing layer claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerError {
    /// Two visible rectangles intersect on the same layer.
    Overlap {
        /// Layer shared by the overlapping claims.
        layer: LayerId,
        /// Identifier of the first claim encountered on that layer.
        first: NodeId,
        /// Identifier of the later claim that intersects `first`.
        second: NodeId,
        /// Visible intersection shared by `first` and `second`.
        intersection: Rect,
    },
    /// A requested node id was not present in the solved layout.
    MissingRect {
        /// Identifier whose solved rectangle was missing.
        id: NodeId,
    },
}

/// Validates that visible rectangles on the same layer do not overlap.
///
/// Empty rectangles are ignored. The first visible same-layer intersection is
/// returned as [`LayerError::Overlap`]. Intersections across different layers
/// are allowed.
pub fn validate_no_overlap(rects: &[LayeredRect]) -> Result<(), LayerError> {
    for (index, first) in rects.iter().enumerate() {
        if first.rect.is_empty() {
            continue;
        }

        for second in &rects[index + 1..] {
            if first.layer != second.layer || second.rect.is_empty() {
                continue;
            }

            if let Some(intersection) = first.rect.intersection(second.rect) {
                return Err(LayerError::Overlap {
                    layer: first.layer,
                    first: first.id,
                    second: second.id,
                    intersection,
                });
            }
        }
    }

    Ok(())
}

/// Builds a layer claim for `id` from a solved layout.
///
/// Returns [`LayerError::MissingRect`] when `layout` has no rectangle for `id`.
pub fn layered_rect(
    layout: &LayoutResult,
    id: NodeId,
    layer: LayerId,
) -> Result<LayeredRect, LayerError> {
    layout
        .get(id)
        .map(|rect| LayeredRect { id, layer, rect })
        .ok_or(LayerError::MissingRect { id })
}
