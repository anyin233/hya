use hya_tui_lib::render::flex::layout;
use hya_tui_lib::{
    Align, FlexDirection, FlexSpec, Justify, LayoutCache, NodeId, Rect, RenderNode, SizeHint,
};

fn leaf(id: u64, flex: FlexSpec) -> RenderNode {
    RenderNode {
        id: Some(NodeId(id)),
        flex,
        children: Vec::new(),
    }
}

fn area(width: u16, height: u16) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

fn rect_of(result: &hya_tui_lib::LayoutResult, id: u64) -> Rect {
    match result.get(NodeId(id)) {
        Some(rect) => rect,
        None => panic!("missing rect for node {id}"),
    }
}

#[test]
fn row_two_percent_halves() {
    let root = RenderNode {
        id: Some(NodeId(0)),
        flex: FlexSpec {
            direction: FlexDirection::Row,
            justify: Justify::Start,
            align: Align::Start,
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

    let result = layout(&root, area(100, 10));

    assert_eq!(
        rect_of(&result, 1),
        Rect {
            x: 0,
            y: 0,
            width: 50,
            height: 10,
        }
    );
    assert_eq!(
        rect_of(&result, 2),
        Rect {
            x: 50,
            y: 0,
            width: 50,
            height: 10,
        }
    );
}

#[test]
fn row_fixed_sidebar_plus_grow_main() {
    let root = RenderNode {
        id: None,
        flex: FlexSpec {
            direction: FlexDirection::Row,
            justify: Justify::Start,
            align: Align::Start,
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

    let result = layout(&root, area(120, 40));

    assert_eq!(
        rect_of(&result, 1),
        Rect {
            x: 0,
            y: 0,
            width: 42,
            height: 40,
        }
    );
    assert_eq!(
        rect_of(&result, 2),
        Rect {
            x: 42,
            y: 0,
            width: 78,
            height: 40,
        }
    );
}

#[test]
fn layout_cache_recomputes_on_area_change() {
    let root = leaf(
        1,
        FlexSpec {
            width: SizeHint::Percent(100.0),
            ..Default::default()
        },
    );
    let mut cache = LayoutCache::new();

    assert_eq!(
        rect_of(cache.layout(&root, area(80, 24)), 1),
        Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        }
    );
    assert_eq!(
        rect_of(cache.layout(&root, area(120, 30)), 1),
        Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 30,
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

    let result = layout(&root, area(80, 40));

    assert_eq!(
        rect_of(&result, 1),
        Rect {
            x: 0,
            y: 15,
            width: 80,
            height: 10,
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

    let result = layout(&root, area(102, 10));

    assert_eq!(rect_of(&result, 1).x, 0);
    assert_eq!(rect_of(&result, 2).x, 32);
    assert_eq!(rect_of(&result, 3).x, 64);
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

    let result = layout(&root, area(100, 10));

    assert_eq!(rect_of(&result, 1).x, 0);
    assert_eq!(rect_of(&result, 2).x, 80);
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

    let result = layout(&root, area(100, 40));

    assert_eq!(
        rect_of(&result, 1),
        Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 1,
        }
    );
    assert_eq!(
        rect_of(&result, 2),
        Rect {
            x: 0,
            y: 1,
            width: 100,
            height: 39,
        }
    );
    assert_eq!(
        rect_of(&result, 3),
        Rect {
            x: 0,
            y: 1,
            width: 42,
            height: 39,
        }
    );
    assert_eq!(
        rect_of(&result, 4),
        Rect {
            x: 42,
            y: 1,
            width: 58,
            height: 39,
        }
    );
}
