//! Public geometry, color, and flex-layout contracts for `hya-tui-lib`.

/// An 8-bit-per-channel RGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// The red channel.
    pub r: u8,
    /// The green channel.
    pub g: u8,
    /// The blue channel.
    pub b: u8,
    /// The alpha channel, where `0` is fully transparent and `255` is fully opaque.
    pub a: u8,
}

impl Rgba {
    /// A fully transparent color.
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);

    /// Creates an opaque RGB color.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    /// Creates an RGBA color with an explicit alpha channel.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parses `#RGB`, `#RRGGBB`, `#RRGGBBAA`, the same strings without `#`,
    /// plus the special values `transparent` and `none`.
    #[must_use]
    pub fn from_hex(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("transparent") || trimmed.eq_ignore_ascii_case("none") {
            return Some(Self::TRANSPARENT);
        }

        let hex = trimmed.strip_prefix('#').unwrap_or(trimmed).as_bytes();
        match hex {
            [r, g, b] => Some(Self::rgb(
                expand_hex_nibble(*r)?,
                expand_hex_nibble(*g)?,
                expand_hex_nibble(*b)?,
            )),
            [r0, r1, g0, g1, b0, b1] => Some(Self::rgb(
                parse_hex_pair(*r0, *r1)?,
                parse_hex_pair(*g0, *g1)?,
                parse_hex_pair(*b0, *b1)?,
            )),
            [r0, r1, g0, g1, b0, b1, a0, a1] => Some(Self::new(
                parse_hex_pair(*r0, *r1)?,
                parse_hex_pair(*g0, *g1)?,
                parse_hex_pair(*b0, *b1)?,
                parse_hex_pair(*a0, *a1)?,
            )),
            _ => None,
        }
    }

    /// Alpha-composites this color over `bg` and returns an opaque result.
    #[must_use]
    pub fn over(self, bg: Self) -> Self {
        match self.a {
            0 => Self::rgb(bg.r, bg.g, bg.b),
            255 => Self::rgb(self.r, self.g, self.b),
            alpha => Self::rgb(
                blend_channel(self.r, bg.r, alpha),
                blend_channel(self.g, bg.g, alpha),
                blend_channel(self.b, bg.b, alpha),
            ),
        }
    }
}

/// A rectangle in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// The left edge in cells.
    pub x: u16,
    /// The top edge in cells.
    pub y: u16,
    /// The width in cells.
    pub width: u16,
    /// The height in cells.
    pub height: u16,
}

impl Rect {
    /// Returns `true` when the rectangle has no visible area.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Returns the exclusive right edge using saturating arithmetic.
    #[must_use]
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Returns the exclusive bottom edge using saturating arithmetic.
    #[must_use]
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Returns the visible overlap between two rectangles, or `None` when they
    /// do not intersect or only touch at empty edges.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        if self.is_empty() || other.is_empty() {
            return None;
        }

        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        if x >= right || y >= bottom {
            return None;
        }

        Some(Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }

    /// Returns `true` when the two rectangles share visible area.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.intersection(other).is_some()
    }

    /// Returns `true` when `other` is fully contained inside this rectangle.
    ///
    /// Empty rectangles are treated as points and are contained when their
    /// origin lies inside this rectangle's bounds.
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        if other.is_empty() {
            return other.x >= self.x
                && other.x <= self.right()
                && other.y >= self.y
                && other.y <= self.bottom();
        }

        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
}

/// The main axis direction for a flex container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexDirection {
    /// Lay children from top to bottom.
    #[default]
    Column,
    /// Lay children from left to right.
    Row,
}

/// How free space is distributed along the main axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    /// Pack children against the start edge.
    #[default]
    Start,
    /// Center children as a group.
    Center,
    /// Pack children against the end edge.
    End,
    /// Put all extra space between children.
    SpaceBetween,
    /// Put half-sized space at the ends and full-sized space between children.
    SpaceAround,
    /// Put equal-sized space before, between, and after children.
    SpaceEvenly,
}

/// How children are positioned on the cross axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// Place children at the cross-axis start edge.
    #[default]
    Start,
    /// Center children on the cross axis.
    Center,
    /// Place children at the cross-axis end edge.
    End,
    /// Stretch automatic cross sizes to the available cross axis.
    Stretch,
}

/// Whether children may wrap onto multiple flex lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrap {
    /// Keep all children on a single line.
    #[default]
    NoWrap,
    /// Request wrapping, which currently debug-asserts and otherwise behaves as `NoWrap`.
    Wrap,
}

/// A size hint for one axis of a flex item.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SizeHint {
    /// Resolve to zero on the main axis and fill the available size on the cross axis.
    #[default]
    Auto,
    /// Use a fixed number of cells.
    Cells(
        /// The requested cell count.
        u16,
    ),
    /// Use a percentage of the parent axis.
    Percent(
        /// The requested percentage, where `100.0` fills the axis.
        f32,
    ),
}

/// The supported flexbox-like layout parameters for a render node.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FlexSpec {
    /// The container main-axis direction.
    pub direction: FlexDirection,
    /// The main-axis free-space distribution rule.
    pub justify: Justify,
    /// The cross-axis alignment rule.
    pub align: Align,
    /// The line-wrapping rule.
    pub wrap: Wrap,
    /// The positive grow weight used when extra main-axis space exists.
    pub grow: f32,
    /// The positive shrink weight used when main-axis space is over-allocated.
    pub shrink: f32,
    /// The fixed gap inserted between adjacent children.
    pub gap: u16,
    /// The width hint for this node.
    pub width: SizeHint,
    /// The height hint for this node.
    pub height: SizeHint,
}

/// A stable identifier for a render node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(
    /// The raw node identifier value.
    pub u64,
);

/// A node in the retained render tree used as layout input.
#[derive(Debug, Clone, Default)]
pub struct RenderNode {
    /// The optional identifier to record in layout output.
    pub id: Option<NodeId>,
    /// The flex layout parameters for this node.
    pub flex: FlexSpec,
    /// The child nodes to solve recursively.
    pub children: Vec<RenderNode>,
}

/// The solved rectangles for every identified render node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutResult {
    /// The `(NodeId, Rect)` pairs produced by layout in traversal order.
    pub rects: Vec<(NodeId, Rect)>,
}

impl LayoutResult {
    /// Returns the solved rectangle for `id`, if that node was recorded.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<Rect> {
        self.rects
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, rect)| *rect)
    }
}

fn blend_channel(foreground: u8, background: u8, alpha: u8) -> u8 {
    let alpha = u16::from(alpha);
    let inverse = 255_u16.saturating_sub(alpha);
    let mixed = u16::from(foreground) * alpha + u16::from(background) * inverse + 127;
    (mixed / 255) as u8
}

fn expand_hex_nibble(byte: u8) -> Option<u8> {
    let nibble = hex_nibble(byte)?;
    Some((nibble << 4) | nibble)
}

fn parse_hex_pair(high: u8, low: u8) -> Option<u8> {
    Some((hex_nibble(high)? << 4) | hex_nibble(low)?)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
