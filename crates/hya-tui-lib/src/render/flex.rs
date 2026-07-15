//! Flex layout solving for retained render trees.

use std::hash::{Hash, Hasher};

use crate::contracts::{
    Align, FlexDirection, FlexSpec, Justify, LayoutResult, Rect, RenderNode, SizeHint, Wrap,
};

/// Solves `root` within `area` and returns rectangles for every identified node.
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
        "flex wrap is unsupported here and falls back to no-wrap"
    );

    let dir = node.flex.direction;
    let child_count = node.children.len();
    if child_count == 0 {
        return Vec::new();
    }

    let available_main = i64::from(main_axis_value(dir, rect.width, rect.height));
    let available_cross = i64::from(cross_axis_value(dir, rect.width, rect.height));
    let gap = i64::from(node.flex.gap);
    let total_gap = gap * (child_count as i64 - 1).max(0);
    let inner_main = (available_main - total_gap).max(0);

    let mut main_sizes: Vec<i64> = node
        .children
        .iter()
        .map(|child| resolve_main(main_hint(dir, &child.flex), available_main))
        .collect();
    let summed_main: i64 = main_sizes.iter().sum();

    if summed_main < inner_main {
        let grow_weights: Vec<f32> = node
            .children
            .iter()
            .map(|child| child.flex.grow.max(0.0))
            .collect();
        let total_grow: f32 = grow_weights.iter().sum();
        if total_grow > 0.0 {
            distribute(
                &mut main_sizes,
                inner_main - summed_main,
                &grow_weights,
                total_grow,
                1,
            );
        }
    } else if summed_main > inner_main {
        let shrink_weights: Vec<f32> = node
            .children
            .iter()
            .map(|child| child.flex.shrink.max(0.0))
            .collect();
        let total_shrink: f32 = shrink_weights.iter().sum();
        if total_shrink > 0.0 {
            distribute(
                &mut main_sizes,
                inner_main - summed_main,
                &shrink_weights,
                total_shrink,
                -1,
            );
        }
    }

    let used_main = main_sizes.iter().sum::<i64>() + total_gap;
    let free_main = (available_main - used_main).max(0);
    let (main_offset, extra_gap) = justify_layout(node.flex.justify, free_main, child_count);

    let main_origin = i64::from(main_axis_value(dir, rect.x, rect.y));
    let mut cursor = main_origin + main_offset;
    let mut out = Vec::with_capacity(child_count);

    for (child, main_size) in node.children.iter().zip(main_sizes) {
        let main_size = main_size.max(0);
        let cross_size = resolve_cross(
            cross_hint(dir, &child.flex),
            available_cross,
            node.flex.align,
        );
        let cross_offset = cross_offset(node.flex.align, available_cross, cross_size);
        out.push(compose_rect(
            dir,
            rect,
            cursor,
            main_size,
            cross_offset,
            cross_size,
        ));
        cursor += main_size + gap + extra_gap;
    }

    out
}

fn distribute(sizes: &mut [i64], delta: i64, weights: &[f32], total_weight: f32, sign: i64) {
    let magnitude = delta.abs();
    let mut assigned = 0_i64;
    let mut last_weighted = None;

    for (index, &weight) in weights.iter().enumerate() {
        if weight <= 0.0 {
            continue;
        }

        let share =
            ((f64::from(weight) / f64::from(total_weight)) * magnitude as f64).floor() as i64;
        sizes[index] = (sizes[index] + sign * share).max(0);
        assigned += share;
        last_weighted = Some(index);
    }

    if let Some(index) = last_weighted {
        let remainder = magnitude - assigned;
        sizes[index] = (sizes[index] + sign * remainder).max(0);
    }
}

fn justify_layout(justify: Justify, free_main: i64, child_count: usize) -> (i64, i64) {
    let child_count = child_count as i64;
    match justify {
        Justify::Start => (0, 0),
        Justify::Center => (free_main / 2, 0),
        Justify::End => (free_main, 0),
        Justify::SpaceBetween if child_count > 1 => (0, free_main / (child_count - 1)),
        Justify::SpaceBetween => (free_main / 2, 0),
        Justify::SpaceAround => {
            let unit = free_main / child_count;
            (unit / 2, unit)
        }
        Justify::SpaceEvenly => {
            let unit = free_main / (child_count + 1);
            (unit, unit)
        }
    }
}

fn main_hint(direction: FlexDirection, flex: &FlexSpec) -> SizeHint {
    match direction {
        FlexDirection::Row => flex.width,
        FlexDirection::Column => flex.height,
    }
}

fn cross_hint(direction: FlexDirection, flex: &FlexSpec) -> SizeHint {
    match direction {
        FlexDirection::Row => flex.height,
        FlexDirection::Column => flex.width,
    }
}

fn resolve_main(hint: SizeHint, available_main: i64) -> i64 {
    match hint {
        SizeHint::Auto => 0,
        SizeHint::Cells(cells) => i64::from(cells).min(available_main),
        SizeHint::Percent(percent) => percent_of(percent, available_main),
    }
}

fn resolve_cross(hint: SizeHint, available_cross: i64, align: Align) -> i64 {
    match hint {
        SizeHint::Auto => available_cross,
        SizeHint::Cells(cells) => i64::from(cells).min(available_cross),
        SizeHint::Percent(percent) => percent_of(percent, available_cross),
    }
    .min(if matches!(align, Align::Stretch) {
        available_cross
    } else {
        i64::MAX
    })
}

fn cross_offset(align: Align, available_cross: i64, child_cross: i64) -> i64 {
    match align {
        Align::Start | Align::Stretch => 0,
        Align::Center => (available_cross - child_cross) / 2,
        Align::End => available_cross - child_cross,
    }
    .max(0)
}

fn percent_of(percent: f32, available: i64) -> i64 {
    ((f64::from(percent) / 100.0) * available as f64).round() as i64
}

fn main_axis_value(direction: FlexDirection, row_value: u16, column_value: u16) -> u16 {
    match direction {
        FlexDirection::Row => row_value,
        FlexDirection::Column => column_value,
    }
}

fn cross_axis_value(direction: FlexDirection, row_value: u16, column_value: u16) -> u16 {
    match direction {
        FlexDirection::Row => column_value,
        FlexDirection::Column => row_value,
    }
}

fn compose_rect(
    direction: FlexDirection,
    rect: Rect,
    main_pos: i64,
    main_size: i64,
    cross_offset: i64,
    cross_size: i64,
) -> Rect {
    match direction {
        FlexDirection::Row => Rect {
            x: clamp_u16(main_pos),
            y: clamp_u16(i64::from(rect.y) + cross_offset),
            width: clamp_u16(main_size),
            height: clamp_u16(cross_size),
        },
        FlexDirection::Column => Rect {
            x: clamp_u16(i64::from(rect.x) + cross_offset),
            y: clamp_u16(main_pos),
            width: clamp_u16(cross_size),
            height: clamp_u16(main_size),
        },
    }
}

fn clamp_u16(value: i64) -> u16 {
    value.clamp(0, i64::from(u16::MAX)) as u16
}

/// Caches the most recent layout result and recomputes when the tree shape or
/// layout area changes.
#[derive(Default)]
pub struct LayoutCache {
    cached: Option<(u64, Rect, LayoutResult)>,
}

impl LayoutCache {
    /// Creates an empty layout cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached layout for `root` in `area`, recomputing when the
    /// render-tree shape hash or area differs from the cached entry.
    pub fn layout(&mut self, root: &RenderNode, area: Rect) -> &LayoutResult {
        let hash = shape_hash(root);
        let is_fresh = matches!(self.cached.as_ref(), Some((cached_hash, cached_area, _)) if *cached_hash == hash && *cached_area == area);
        if !is_fresh {
            self.cached = Some((hash, area, layout(root, area)));
        }

        let (_, _, result) = self
            .cached
            .get_or_insert_with(|| (hash, area, layout(root, area)));
        result
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
        SizeHint::Auto => {}
        SizeHint::Cells(cells) => cells.hash(hasher),
        SizeHint::Percent(percent) => percent.to_bits().hash(hasher),
    }
}
