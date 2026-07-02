//! Declarative component trees backed by flex layout and layer validation.

use std::collections::HashSet;

use crate::contracts::{FlexSpec, LayoutResult, NodeId, Rect, RenderNode};
use crate::layer::{LayerError, LayerId, LayeredRect, layered_rect, validate_no_overlap};

/// Distinguishes layout-only containers from painting leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// A layout node that may have an id for lookup but does not paint.
    Container,
    /// A layout node that paints its solved rectangle.
    Leaf,
}

/// A declarative layout node that can be solved into rectangles and paint claims.
#[derive(Debug, Clone)]
pub struct Component {
    kind: ComponentKind,
    id: Option<NodeId>,
    flex: FlexSpec,
    layer: LayerId,
    children: Vec<Component>,
}

/// Solved component rectangles plus sorted paint claims for leaf nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentLayout {
    rects: Vec<(NodeId, Rect)>,
    layered: Vec<LayeredRect>,
}

/// Reports invalid component trees or missing solved rectangles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentError {
    /// The component tree reused a `NodeId`.
    DuplicateId(
        /// Identifier that appeared more than once in the tree.
        NodeId,
    ),
    /// A requested `NodeId` was absent from the solved layout.
    MissingRect(
        /// Identifier whose solved rectangle was missing.
        NodeId,
    ),
    /// Layer validation failed for one or more leaf paint claims.
    Layer(
        /// The underlying layer validation error.
        LayerError,
    ),
}

impl Component {
    /// Creates a non-painting container node with the provided flex spec.
    #[must_use]
    pub fn container(flex: FlexSpec) -> Self {
        Self {
            kind: ComponentKind::Container,
            id: None,
            flex,
            layer: LayerId::base(),
            children: Vec::new(),
        }
    }

    /// Creates a painting leaf node on the base layer.
    #[must_use]
    pub fn leaf(id: NodeId, flex: FlexSpec) -> Self {
        Self {
            kind: ComponentKind::Leaf,
            id: Some(id),
            flex,
            layer: LayerId::base(),
            children: Vec::new(),
        }
    }

    /// Assigns or replaces the lookup id for this component.
    #[must_use]
    pub fn id(mut self, id: NodeId) -> Self {
        self.id = Some(id);
        self
    }

    /// Overrides the paint layer used when this component produces a paint claim.
    #[must_use]
    pub fn layer(mut self, layer: LayerId) -> Self {
        self.layer = layer;
        self
    }

    /// Appends one child component and returns the updated tree.
    #[must_use]
    pub fn child(mut self, child: Component) -> Self {
        self.children.push(child);
        self
    }

    /// Appends child components in declaration order and returns the updated tree.
    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = Component>) -> Self {
        self.children.extend(children);
        self
    }

    /// Solves layout for the component tree within `area`.
    ///
    /// Returns [`ComponentError::DuplicateId`] when any node id is reused,
    /// [`ComponentError::MissingRect`] when an expected solved rectangle is absent,
    /// and [`ComponentError::Layer`] when same-layer leaf claims overlap.
    pub fn layout(&self, area: Rect) -> Result<ComponentLayout, ComponentError> {
        let mut seen_ids = HashSet::new();
        let mut declared_leaves = Vec::new();
        let root = self.render_node(&mut seen_ids, &mut declared_leaves)?;
        let layout = crate::render::flex::layout(&root, area);
        let layered = build_layered(&layout, &declared_leaves)?;
        validate_no_overlap(&layered)?;

        Ok(ComponentLayout {
            rects: layout.rects,
            layered,
        })
    }

    fn render_node(
        &self,
        seen_ids: &mut HashSet<NodeId>,
        declared_leaves: &mut Vec<DeclaredLeaf>,
    ) -> Result<RenderNode, ComponentError> {
        if let Some(id) = self.id
            && !seen_ids.insert(id)
        {
            return Err(ComponentError::DuplicateId(id));
        }

        if matches!(self.kind, ComponentKind::Leaf)
            && let Some(id) = self.id
        {
            declared_leaves.push(DeclaredLeaf {
                id,
                layer: self.layer,
            });
        }

        let children = self
            .children
            .iter()
            .map(|child| child.render_node(seen_ids, declared_leaves))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(RenderNode {
            id: self.id,
            flex: self.flex,
            children,
        })
    }
}

impl ComponentLayout {
    /// Returns the solved rectangle for `id`, including layout-only container ids.
    #[must_use]
    pub fn rect(&self, id: NodeId) -> Option<Rect> {
        self.rects
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, rect)| *rect)
    }

    /// Returns the solved rectangle for `id` or a typed missing-rectangle error.
    pub fn require_rect(&self, id: NodeId) -> Result<Rect, ComponentError> {
        self.rect(id).ok_or(ComponentError::MissingRect(id))
    }

    /// Returns sorted paint claims for leaf nodes only.
    #[must_use]
    pub fn layered(&self) -> &[LayeredRect] {
        &self.layered
    }
}

impl From<LayerError> for ComponentError {
    fn from(value: LayerError) -> Self {
        match value {
            LayerError::MissingRect { id } => Self::MissingRect(id),
            other => Self::Layer(other),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DeclaredLeaf {
    id: NodeId,
    layer: LayerId,
}

fn build_layered(
    layout: &LayoutResult,
    declared_leaves: &[DeclaredLeaf],
) -> Result<Vec<LayeredRect>, LayerError> {
    let mut layered = declared_leaves
        .iter()
        .enumerate()
        .map(|(order, declared)| {
            layered_rect(layout, declared.id, declared.layer)
                .map(|claim| (declared.layer, order, claim))
        })
        .collect::<Result<Vec<_>, _>>()?;

    layered.sort_unstable_by_key(|(layer, order, _)| (*layer, *order));

    Ok(layered.into_iter().map(|(_, _, claim)| claim).collect())
}
