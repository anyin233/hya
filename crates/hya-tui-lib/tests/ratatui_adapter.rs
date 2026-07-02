#![allow(clippy::expect_used)]

use hya_tui_lib::Rect;
use hya_tui_lib::Rgba;
use hya_tui_lib::component::Component;
use hya_tui_lib::contracts::{FlexDirection, FlexSpec, NodeId, SizeHint};
use hya_tui_lib::render::draw::{rect_to_ratatui, rgba_to_color};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::widgets::Paragraph;

fn buffer_text(buffer: &Buffer, width: u16, height: u16) -> String {
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn rgba_to_color_blends_alpha_over_background() {
    let color = rgba_to_color(Rgba::new(255, 255, 255, 0x80), Rgba::rgb(0, 0, 0));

    assert_eq!(color, Color::Rgb(128, 128, 128));
}

#[test]
fn rect_to_ratatui_preserves_geometry() {
    let rect = rect_to_ratatui(Rect {
        x: 1,
        y: 2,
        width: 3,
        height: 4,
    });

    assert_eq!(rect.x, 1);
    assert_eq!(rect.y, 2);
    assert_eq!(rect.width, 3);
    assert_eq!(rect.height, 4);
}

#[test]
fn declared_components_render_into_test_backend() {
    let layout = Component::container(FlexSpec {
        direction: FlexDirection::Row,
        ..Default::default()
    })
    .children([
        Component::leaf(
            NodeId(1),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..Default::default()
            },
        ),
        Component::leaf(
            NodeId(2),
            FlexSpec {
                width: SizeHint::Percent(50.0),
                ..Default::default()
            },
        ),
    ])
    .layout(Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 1,
    })
    .expect("component layout should succeed");

    let left = rect_to_ratatui(
        layout
            .require_rect(NodeId(1))
            .expect("left component rect should exist"),
    );
    let right = rect_to_ratatui(
        layout
            .require_rect(NodeId(2))
            .expect("right component rect should exist"),
    );

    let backend = TestBackend::new(20, 1);
    let mut terminal = Terminal::new(backend).expect("test backend should initialize");

    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new("left"), left);
            frame.render_widget(Paragraph::new("right"), right);
        })
        .expect("render should succeed");

    let rendered = buffer_text(terminal.backend().buffer(), 20, 1);
    assert!(rendered.contains("left"));
    assert!(rendered.contains("right"));
}
