//! Flexbox layout solver (PLAN.md W3a, retires R3).
//!
//! Computes a paint-independent rect table from a [`RenderNode`] tree, emulating the subset of
//! opentui's Yoga flexbox the TUI actually uses.
//!
//! Supported (frozen matrix): nested row/column containers; `width`/`height` as fixed cells or
//! percent; `flex_grow` / `flex_shrink`; `gap`; `justify` (start/center/end/space-*); `align`
//! (start/center/end/stretch); absolute child rects via recursion.
//!
//! NOT supported (frozen): `flex_wrap` (single line only — `debug_assert` flags misuse, release
//! falls back to no-wrap), and intrinsic/content-based `Auto` sizing (an `Auto` main size is 0
//! plus whatever `flex_grow` distributes — callers give explicit sizes or use grow). Content
//! measurement belongs to the widget layer (W3c), not the layout solver.

use std::hash::{Hash, Hasher};

use crate::contracts::{
    Align, FlexDirection, FlexSpec, Justify, LayoutResult, Rect, RenderNode, SizeHint, Wrap,
};

/// Solve layout for `root` within `area`, returning the rect of every node that has an id.
#[must_use]
pub fn layout(root: &RenderNode, area: Rect) -> LayoutResult {
    let mut out = LayoutResult::default();
    place(root, area, &mut out);
    out
}

fn place(node: &RenderNode, rect: Rect, out: &mut LayoutResult) {
    if let Some(id) = node.id {
        out.rects.push((id, rect));
    }
    if node.children.is_empty() {
        return;
    }
    for (child, child_rect) in node.children.iter().zip(arrange(node, rect)) {
        place(child, child_rect, out);
    }
}

fn arrange(node: &RenderNode, rect: Rect) -> Vec<Rect> {
    debug_assert!(
        matches!(node.flex.wrap, Wrap::NoWrap),
        "flex_wrap is outside the W3a support matrix; falling back to no-wrap"
    );
    let dir = node.flex.direction;
    let n = node.children.len();
    if n == 0 {
        return Vec::new();
    }
    let avail_main = i64::from(main(dir, rect.width, rect.height));
    let avail_cross = i64::from(cross(dir, rect.width, rect.height));
    let gap = i64::from(node.flex.gap);
    let gaps_total = gap * (n as i64 - 1).max(0);
    let inner_main = (avail_main - gaps_total).max(0);

    let mut sizes: Vec<i64> = node
        .children
        .iter()
        .map(|c| resolve_main(main_hint(dir, &c.flex), avail_main))
        .collect();
    let sum: i64 = sizes.iter().sum();

    if sum < inner_main {
        let grows: Vec<f32> = node.children.iter().map(|c| c.flex.grow.max(0.0)).collect();
        let total: f32 = grows.iter().sum();
        if total > 0.0 {
            distribute(&mut sizes, inner_main - sum, &grows, total, 1);
        }
    } else if sum > inner_main {
        let shrinks: Vec<f32> = node
            .children
            .iter()
            .map(|c| c.flex.shrink.max(0.0))
            .collect();
        let total: f32 = shrinks.iter().sum();
        if total > 0.0 {
            distribute(&mut sizes, inner_main - sum, &shrinks, total, -1);
        }
    }

    let used: i64 = sizes.iter().sum::<i64>() + gaps_total;
    let free = (avail_main - used).max(0);
    let (start_off, extra) = justify_layout(node.flex.justify, free, n);

    let main_origin = i64::from(main(dir, rect.x, rect.y));
    let mut cursor = main_origin + start_off;
    let mut rects = Vec::with_capacity(n);
    for child in &node.children {
        let m_size = sizes.remove(0).max(0);
        let c_size = resolve_cross(cross_hint(dir, &child.flex), avail_cross, node.flex.align);
        let c_off = cross_offset(node.flex.align, avail_cross, c_size);
        rects.push(compose(dir, rect, cursor, m_size, c_off, c_size));
        cursor += m_size + gap + extra;
    }
    rects
}

/// Distribute `delta` (positive for grow, negative for shrink) across `sizes` by `weights`.
/// `sign` is +1 (grow) or -1 (shrink); rounding remainder lands on the last weighted child.
fn distribute(sizes: &mut [i64], delta: i64, weights: &[f32], total: f32, sign: i64) {
    let magnitude = delta.abs();
    let mut assigned = 0i64;
    let mut last = None;
    for (i, &w) in weights.iter().enumerate() {
        if w <= 0.0 {
            continue;
        }
        let share = ((f64::from(w) / f64::from(total)) * magnitude as f64).floor() as i64;
        sizes[i] = (sizes[i] + sign * share).max(0);
        assigned += share;
        last = Some(i);
    }
    if let Some(i) = last {
        let remainder = magnitude - assigned;
        sizes[i] = (sizes[i] + sign * remainder).max(0);
    }
}

fn justify_layout(justify: Justify, free: i64, n: usize) -> (i64, i64) {
    let count = n as i64;
    match justify {
        Justify::Start => (0, 0),
        Justify::Center => (free / 2, 0),
        Justify::End => (free, 0),
        Justify::SpaceBetween if count > 1 => (0, free / (count - 1)),
        Justify::SpaceBetween => (free / 2, 0),
        Justify::SpaceAround => {
            let unit = free / count;
            (unit / 2, unit)
        }
        Justify::SpaceEvenly => {
            let unit = free / (count + 1);
            (unit, unit)
        }
    }
}

fn main_hint(dir: FlexDirection, flex: &FlexSpec) -> SizeHint {
    match dir {
        FlexDirection::Row => flex.width,
        FlexDirection::Column => flex.height,
    }
}

fn cross_hint(dir: FlexDirection, flex: &FlexSpec) -> SizeHint {
    match dir {
        FlexDirection::Row => flex.height,
        FlexDirection::Column => flex.width,
    }
}

fn resolve_main(hint: SizeHint, avail: i64) -> i64 {
    match hint {
        SizeHint::Auto => 0,
        SizeHint::Cells(c) => i64::from(c).min(avail),
        SizeHint::Percent(p) => percent_of(p, avail),
    }
}

fn resolve_cross(hint: SizeHint, avail: i64, align: Align) -> i64 {
    match hint {
        SizeHint::Auto => avail,
        SizeHint::Cells(c) => i64::from(c).min(avail),
        SizeHint::Percent(p) => percent_of(p, avail),
    }
    .min(if matches!(align, Align::Stretch) {
        avail
    } else {
        i64::MAX
    })
}

fn cross_offset(align: Align, avail: i64, size: i64) -> i64 {
    match align {
        Align::Start | Align::Stretch => 0,
        Align::Center => (avail - size) / 2,
        Align::End => avail - size,
    }
    .max(0)
}

fn percent_of(p: f32, avail: i64) -> i64 {
    ((f64::from(p) / 100.0) * avail as f64).round() as i64
}

fn main(dir: FlexDirection, x_or_w: u16, y_or_h: u16) -> u16 {
    match dir {
        FlexDirection::Row => x_or_w,
        FlexDirection::Column => y_or_h,
    }
}

fn cross(dir: FlexDirection, x_or_w: u16, y_or_h: u16) -> u16 {
    match dir {
        FlexDirection::Row => y_or_h,
        FlexDirection::Column => x_or_w,
    }
}

fn compose(
    dir: FlexDirection,
    rect: Rect,
    main_pos: i64,
    main_sz: i64,
    cross_off: i64,
    cross_sz: i64,
) -> Rect {
    match dir {
        FlexDirection::Row => Rect {
            x: clamp_u16(main_pos),
            y: clamp_u16(i64::from(rect.y) + cross_off),
            width: clamp_u16(main_sz),
            height: clamp_u16(cross_sz),
        },
        FlexDirection::Column => Rect {
            x: clamp_u16(i64::from(rect.x) + cross_off),
            y: clamp_u16(main_pos),
            width: clamp_u16(cross_sz),
            height: clamp_u16(main_sz),
        },
    }
}

fn clamp_u16(v: i64) -> u16 {
    v.clamp(0, i64::from(u16::MAX)) as u16
}

/// Caches the last layout result, recomputing only when the tree shape or area changes
/// (PLAN.md W3a — avoids re-solving every frame when nothing structural changed).
#[derive(Default)]
pub struct LayoutCache {
    cached: Option<(u64, Rect, LayoutResult)>,
}

impl LayoutCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn layout(&mut self, root: &RenderNode, area: Rect) -> &LayoutResult {
        let hash = shape_hash(root);
        let fresh = matches!(&self.cached, Some((h, a, _)) if *h == hash && *a == area);
        if !fresh {
            self.cached = Some((hash, area, layout(root, area)));
        }
        match &self.cached {
            Some((_, _, result)) => result,
            None => unreachable!("cache populated immediately above"),
        }
    }
}

fn shape_hash(node: &RenderNode) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_node(node, &mut hasher);
    hasher.finish()
}

fn hash_node(node: &RenderNode, hasher: &mut impl Hasher) {
    node.id.map(|id| id.0).hash(hasher);
    hash_flex(&node.flex, hasher);
    node.children.len().hash(hasher);
    for child in &node.children {
        hash_node(child, hasher);
    }
}

fn hash_flex(flex: &FlexSpec, hasher: &mut impl Hasher) {
    std::mem::discriminant(&flex.direction).hash(hasher);
    std::mem::discriminant(&flex.justify).hash(hasher);
    std::mem::discriminant(&flex.align).hash(hasher);
    std::mem::discriminant(&flex.wrap).hash(hasher);
    flex.grow.to_bits().hash(hasher);
    flex.shrink.to_bits().hash(hasher);
    flex.gap.hash(hasher);
    hash_size(flex.width, hasher);
    hash_size(flex.height, hasher);
}

fn hash_size(size: SizeHint, hasher: &mut impl Hasher) {
    std::mem::discriminant(&size).hash(hasher);
    match size {
        SizeHint::Cells(c) => c.hash(hasher),
        SizeHint::Percent(p) => p.to_bits().hash(hasher),
        SizeHint::Auto => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::NodeId;

    fn leaf(id: u64, flex: FlexSpec) -> RenderNode {
        RenderNode {
            id: Some(NodeId(id)),
            flex,
            children: Vec::new(),
        }
    }

    fn area(w: u16, h: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    fn rect_of(result: &LayoutResult, id: u64) -> Rect {
        result.get(NodeId(id)).expect("node id present in layout")
    }

    #[test]
    fn row_two_percent_halves() {
        let root = RenderNode {
            id: Some(NodeId(0)),
            flex: FlexSpec {
                direction: FlexDirection::Row,
                ..Default::default()
            },
            children: vec![
                leaf(
                    1,
                    FlexSpec {
                        width: SizeHint::Percent(50.0),
                        ..Default::default()
                    },
                ),
                leaf(
                    2,
                    FlexSpec {
                        width: SizeHint::Percent(50.0),
                        ..Default::default()
                    },
                ),
            ],
        };
        let r = layout(&root, area(100, 10));
        assert_eq!(
            rect_of(&r, 1),
            Rect {
                x: 0,
                y: 0,
                width: 50,
                height: 10
            }
        );
        assert_eq!(
            rect_of(&r, 2),
            Rect {
                x: 50,
                y: 0,
                width: 50,
                height: 10
            }
        );
    }

    #[test]
    fn row_fixed_sidebar_plus_grow_main() {
        // Mirrors the session layout: a 42-cell sidebar + a main pane that grows.
        let root = RenderNode {
            id: None,
            flex: FlexSpec {
                direction: FlexDirection::Row,
                ..Default::default()
            },
            children: vec![
                leaf(
                    1,
                    FlexSpec {
                        width: SizeHint::Cells(42),
                        ..Default::default()
                    },
                ),
                leaf(
                    2,
                    FlexSpec {
                        grow: 1.0,
                        ..Default::default()
                    },
                ),
            ],
        };
        let r = layout(&root, area(120, 40));
        assert_eq!(
            rect_of(&r, 1),
            Rect {
                x: 0,
                y: 0,
                width: 42,
                height: 40
            }
        );
        assert_eq!(
            rect_of(&r, 2),
            Rect {
                x: 42,
                y: 0,
                width: 78,
                height: 40
            }
        );
    }

    #[test]
    fn column_center_justifies_single_child() {
        let root = RenderNode {
            id: None,
            flex: FlexSpec {
                direction: FlexDirection::Column,
                justify: Justify::Center,
                ..Default::default()
            },
            children: vec![leaf(
                1,
                FlexSpec {
                    height: SizeHint::Cells(10),
                    width: SizeHint::Percent(100.0),
                    ..Default::default()
                },
            )],
        };
        let r = layout(&root, area(80, 40));
        assert_eq!(
            rect_of(&r, 1),
            Rect {
                x: 0,
                y: 15,
                width: 80,
                height: 10
            }
        );
    }

    #[test]
    fn row_gap_between_fixed_children() {
        let root = RenderNode {
            id: None,
            flex: FlexSpec {
                direction: FlexDirection::Row,
                gap: 2,
                ..Default::default()
            },
            children: vec![
                leaf(
                    1,
                    FlexSpec {
                        width: SizeHint::Cells(30),
                        ..Default::default()
                    },
                ),
                leaf(
                    2,
                    FlexSpec {
                        width: SizeHint::Cells(30),
                        ..Default::default()
                    },
                ),
                leaf(
                    3,
                    FlexSpec {
                        width: SizeHint::Cells(30),
                        ..Default::default()
                    },
                ),
            ],
        };
        let r = layout(&root, area(102, 10));
        assert_eq!(rect_of(&r, 1).x, 0);
        assert_eq!(rect_of(&r, 2).x, 32);
        assert_eq!(rect_of(&r, 3).x, 64);
    }

    #[test]
    fn row_space_between_pushes_to_edges() {
        let root = RenderNode {
            id: None,
            flex: FlexSpec {
                direction: FlexDirection::Row,
                justify: Justify::SpaceBetween,
                ..Default::default()
            },
            children: vec![
                leaf(
                    1,
                    FlexSpec {
                        width: SizeHint::Cells(20),
                        ..Default::default()
                    },
                ),
                leaf(
                    2,
                    FlexSpec {
                        width: SizeHint::Cells(20),
                        ..Default::default()
                    },
                ),
            ],
        };
        let r = layout(&root, area(100, 10));
        assert_eq!(rect_of(&r, 1).x, 0);
        assert_eq!(rect_of(&r, 2).x, 80);
    }

    #[test]
    fn nested_column_then_row_recurses() {
        let root = RenderNode {
            id: Some(NodeId(0)),
            flex: FlexSpec {
                direction: FlexDirection::Column,
                ..Default::default()
            },
            children: vec![
                leaf(
                    1,
                    FlexSpec {
                        height: SizeHint::Cells(1),
                        ..Default::default()
                    },
                ),
                RenderNode {
                    id: Some(NodeId(2)),
                    flex: FlexSpec {
                        direction: FlexDirection::Row,
                        grow: 1.0,
                        ..Default::default()
                    },
                    children: vec![
                        leaf(
                            3,
                            FlexSpec {
                                width: SizeHint::Cells(42),
                                ..Default::default()
                            },
                        ),
                        leaf(
                            4,
                            FlexSpec {
                                grow: 1.0,
                                ..Default::default()
                            },
                        ),
                    ],
                },
            ],
        };
        let r = layout(&root, area(100, 40));
        assert_eq!(
            rect_of(&r, 1),
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 1
            }
        );
        assert_eq!(
            rect_of(&r, 2),
            Rect {
                x: 0,
                y: 1,
                width: 100,
                height: 39
            }
        );
        assert_eq!(
            rect_of(&r, 3),
            Rect {
                x: 0,
                y: 1,
                width: 42,
                height: 39
            }
        );
        assert_eq!(
            rect_of(&r, 4),
            Rect {
                x: 42,
                y: 1,
                width: 58,
                height: 39
            }
        );
    }

    #[test]
    fn cache_reuses_then_recomputes_on_area_change() {
        let root = leaf(
            1,
            FlexSpec {
                width: SizeHint::Percent(100.0),
                ..Default::default()
            },
        );
        let mut cache = LayoutCache::new();
        let first = cache.layout(&root, area(80, 24)).get(NodeId(1));
        assert_eq!(
            first,
            Some(Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24
            })
        );
        let again = cache.layout(&root, area(80, 24)).get(NodeId(1));
        assert_eq!(again, first);
        let resized = cache.layout(&root, area(120, 30)).get(NodeId(1));
        assert_eq!(
            resized,
            Some(Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 30
            })
        );
    }
}
