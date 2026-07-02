#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_tui_lib::component::{Component, ComponentError};
use hya_tui_lib::layer::{LayerError, layered_rect, validate_no_overlap};
use hya_tui_lib::{FlexDirection, FlexSpec, LayerId, LayeredRect, NodeId, Rect, SizeHint};

#[test]
fn same_layer_overlap_returns_typed_error() {
    let claims = [
        LayeredRect {
            id: NodeId(1),
            layer: LayerId::base(),
            rect: Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        },
        LayeredRect {
            id: NodeId(2),
            layer: LayerId::base(),
            rect: Rect {
                x: 5,
                y: 0,
                width: 10,
                height: 10,
            },
        },
    ];

    assert_eq!(
        validate_no_overlap(&claims),
        Err(LayerError::Overlap {
            layer: LayerId::base(),
            first: NodeId(1),
            second: NodeId(2),
            intersection: Rect {
                x: 5,
                y: 0,
                width: 5,
                height: 10,
            },
        })
    );
}

#[test]
fn different_layers_may_overlap() {
    let claims = [
        LayeredRect {
            id: NodeId(1),
            layer: LayerId::base(),
            rect: Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        },
        LayeredRect {
            id: NodeId(2),
            layer: LayerId(1),
            rect: Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        },
    ];

    assert_eq!(validate_no_overlap(&claims), Ok(()));
}

#[test]
fn component_tree_row_layout_returns_leaf_claims() {
    let component = Component::container(FlexSpec {
        direction: FlexDirection::Row,
        ..FlexSpec::default()
    })
    .children([
        Component::leaf(
            NodeId(1),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..FlexSpec::default()
            },
        ),
        Component::leaf(
            NodeId(2),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..FlexSpec::default()
            },
        ),
    ]);
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 10,
    };

    let layout = component.layout(area).expect("row layout should succeed");
    let expected = [
        LayeredRect {
            id: NodeId(1),
            layer: LayerId::base(),
            rect: Rect {
                x: 0,
                y: 0,
                width: 50,
                height: 10,
            },
        },
        LayeredRect {
            id: NodeId(2),
            layer: LayerId::base(),
            rect: Rect {
                x: 50,
                y: 0,
                width: 50,
                height: 10,
            },
        },
    ];

    assert_eq!(
        layout.rect(NodeId(1)),
        Some(Rect {
            x: 0,
            y: 0,
            width: 50,
            height: 10,
        })
    );
    assert_eq!(
        layout.rect(NodeId(2)),
        Some(Rect {
            x: 50,
            y: 0,
            width: 50,
            height: 10,
        })
    );
    assert_eq!(layout.layered(), &expected[..]);
}

#[test]
fn component_container_id_is_lookup_only_not_paint_claim() {
    let component = Component::container(FlexSpec::default())
        .id(NodeId(10))
        .child(Component::leaf(
            NodeId(1),
            FlexSpec {
                width: SizeHint::Percent(100.0),
                height: SizeHint::Percent(100.0),
                ..FlexSpec::default()
            },
        ));
    let area = Rect {
        x: 4,
        y: 2,
        width: 20,
        height: 6,
    };

    let layout = component
        .layout(area)
        .expect("container id layout should succeed");
    let expected = [LayeredRect {
        id: NodeId(1),
        layer: LayerId::base(),
        rect: area,
    }];

    assert_eq!(layout.rect(NodeId(10)), Some(area));
    assert_eq!(layout.layered(), &expected[..]);
}

#[test]
fn component_leaf_layer_override_sorts_paint_claims() {
    let component = Component::container(FlexSpec {
        direction: FlexDirection::Row,
        ..FlexSpec::default()
    })
    .children([
        Component::leaf(
            NodeId(1),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..FlexSpec::default()
            },
        )
        .layer(LayerId(1)),
        Component::leaf(
            NodeId(2),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..FlexSpec::default()
            },
        ),
    ]);
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 10,
    };

    let layout = component
        .layout(area)
        .expect("layer override layout should succeed");
    let expected = [
        LayeredRect {
            id: NodeId(2),
            layer: LayerId::base(),
            rect: Rect {
                x: 50,
                y: 0,
                width: 50,
                height: 10,
            },
        },
        LayeredRect {
            id: NodeId(1),
            layer: LayerId(1),
            rect: Rect {
                x: 0,
                y: 0,
                width: 50,
                height: 10,
            },
        },
    ];

    assert_eq!(layout.layered(), &expected[..]);
}

#[test]
fn layered_rect_missing_layout_id_returns_missing_rect() {
    let layout = hya_tui_lib::LayoutResult::default();

    assert_eq!(
        layered_rect(&layout, NodeId(99), LayerId::base()),
        Err(LayerError::MissingRect { id: NodeId(99) })
    );
}

#[test]
fn component_layout_require_rect_returns_missing_rect() {
    let component = Component::container(FlexSpec::default()).child(Component::leaf(
        NodeId(1),
        FlexSpec {
            width: SizeHint::Percent(100.0),
            height: SizeHint::Percent(100.0),
            ..FlexSpec::default()
        },
    ));
    let layout = component
        .layout(Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        })
        .expect("layout should succeed");

    assert_eq!(
        layout.require_rect(NodeId(99)),
        Err(ComponentError::MissingRect(NodeId(99)))
    );
}

#[test]
fn component_layout_rejects_duplicate_ids() {
    let component = Component::container(FlexSpec {
        direction: FlexDirection::Row,
        ..FlexSpec::default()
    })
    .children([
        Component::leaf(
            NodeId(1),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..FlexSpec::default()
            },
        ),
        Component::leaf(
            NodeId(1),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..FlexSpec::default()
            },
        ),
    ]);

    assert_eq!(
        component.layout(Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 10,
        }),
        Err(ComponentError::DuplicateId(NodeId(1)))
    );
}
