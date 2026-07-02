use hya_tui_lib::Rect;

#[test]
fn rect_intersection_ignores_empty_edges() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 10,
        height: 10,
    };
    let touching = Rect {
        x: 10,
        y: 0,
        width: 10,
        height: 10,
    };
    let overlapping = Rect {
        x: 5,
        y: 6,
        width: 10,
        height: 10,
    };

    assert_eq!(area.intersection(touching), None);
    assert!(!area.intersects(touching));

    let intersection = Rect {
        x: 5,
        y: 6,
        width: 5,
        height: 4,
    };

    assert_eq!(area.intersection(overlapping), Some(intersection));
    assert!(area.contains(intersection));
    assert!(overlapping.contains(intersection));
}
