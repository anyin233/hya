use hya_tui_lib::Rect;
use hya_tui_lib::render::overlay::centered_rect;

#[test]
fn centered_rect_clamps_inside_area() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    assert_eq!(
        centered_rect(200, 50, area),
        Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        }
    );
}
