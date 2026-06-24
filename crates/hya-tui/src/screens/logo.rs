//! Shadow markers (rendered char in parens):
//! - `_` = full shadow cell (space with bg=shadow)
//! - `^` = letter top, shadow bottom (▀ with fg=letter, bg=shadow)
//! - `~` = shadow top only (▀ with fg=shadow)
//! - `,` = shadow bottom only (▄ with fg=shadow)

use crate::contracts::Rgba;
use crate::render::text::{Attrs, Line, Span, Text};
use crate::theme::{tint, ResolvedTheme};

/// "HYA" wordmark, 14 cells wide. All three glyphs use the muted "open" treatment
/// (`logo.ts` left half) plus the shimmer — no bold "code" half.
const GLYPHS: [&str; 4] = [
    "              ",
    "█__█ █__█ ▄▀▀▄",
    "█^^█  ██  █^^█",
    "▀  ▀  ▀▀  ▀  ▀",
];

pub const LOGO_WIDTH: u16 = 14;
pub const LOGO_HEIGHT: u16 = 4;

/// `tint(background, ink, 0.25)` — the shadow color used for the `_^~,` markers.
const SHADOW_ALPHA: f32 = 0.25;

const SHIMMER_PERIOD: f32 = 4_600.0;
const SHIMMER_RINGS: usize = 2;
const SHIMMER_CORE_WIDTH: f32 = 1.2;
const SHIMMER_CORE_AMP: f32 = 1.9;
const SHIMMER_TAIL: f32 = 5.0;
const SHIMMER_HALO_WIDTH: f32 = 4.3;
const SHIMMER_HALO_OFFSET: f32 = 0.6;
const SHIMMER_HALO_AMP: f32 = 0.16;
const SHIMMER_BREATH_BASE: f32 = 0.04;
const SHIMMER_NOISE: f32 = 0.1;
const SHIMMER_AMBIENT_AMP: f32 = 0.36;
const SHIMMER_AMBIENT_CENTER: f32 = 0.5;
const SHIMMER_AMBIENT_WIDTH: f32 = 0.34;
const SHIMMER_PRIMARY_MIX: f32 = 0.3;
const SHIMMER_ORIGIN_X: f32 = 4.5;
const SHIMMER_ORIGIN_Y: f32 = 13.5;
const PEAK: Rgba = Rgba::rgb(255, 255, 255);

#[must_use]
pub fn logo_text(theme: &ResolvedTheme) -> Text {
    let lines = (0..4)
        .map(|row| Line(render_half(GLYPHS[row], theme.text_muted, false, theme.background)))
        .collect();
    Text(lines)
}

#[must_use]
pub fn logo_text_at(theme: &ResolvedTheme, elapsed: std::time::Duration) -> Text {
    if elapsed.is_zero() {
        return logo_text(theme);
    }

    let elapsed_ms = elapsed.as_secs_f32() * 1_000.0;
    let reach = shimmer_reach();
    let lines = (0..4)
        .map(|row| {
            Line(render_half_at(
                GLYPHS[row],
                RenderHalfAt {
                    theme,
                    ink: theme.text_muted,
                    bold: false,
                    row,
                    x_offset: 0,
                    elapsed_ms,
                    reach,
                },
            ))
        })
        .collect();
    Text(lines)
}

fn render_half(
    line: &str,
    ink: crate::contracts::Rgba,
    bold: bool,
    background: crate::contracts::Rgba,
) -> Vec<Span> {
    let shadow = tint(background, ink, SHADOW_ALPHA);
    let attrs = Attrs {
        bold,
        ..Attrs::default()
    };
    line.chars()
        .map(|ch| match ch {
            '_' => Span::styled(" ", Some(ink), Some(shadow), attrs),
            '^' => Span::styled("▀", Some(ink), Some(shadow), attrs),
            '~' => Span::styled("▀", Some(shadow), None, attrs),
            ',' => Span::styled("▄", Some(shadow), None, attrs),
            other => Span::styled(other.to_string(), Some(ink), None, attrs),
        })
        .collect()
}

#[derive(Clone, Copy)]
struct RenderHalfAt<'a> {
    theme: &'a ResolvedTheme,
    ink: Rgba,
    bold: bool,
    row: usize,
    x_offset: usize,
    elapsed_ms: f32,
    reach: f32,
}

fn render_half_at(line: &str, render: RenderHalfAt<'_>) -> Vec<Span> {
    let shadow = tint(render.theme.background, render.ink, SHADOW_ALPHA);
    let attrs = Attrs {
        bold: render.bold,
        ..Attrs::default()
    };

    line.chars()
        .enumerate()
        .map(|(i, ch)| {
            let x = render.x_offset + i;
            match ch {
                '_' => Span::styled(" ", Some(render.ink), Some(shadow), attrs),
                '^' => Span::styled(
                    "▀",
                    Some(ink_at(x, render.row * 2, render)),
                    Some(shadow),
                    attrs,
                ),
                '~' => Span::styled("▀", Some(shadow), None, attrs),
                ',' => Span::styled("▄", Some(shadow), None, attrs),
                ' ' => Span::styled(" ", Some(render.ink), None, attrs),
                '▀' => Span::styled("▀", Some(ink_at(x, render.row * 2, render)), None, attrs),
                '▄' => Span::styled(
                    "▄",
                    Some(ink_at(x, render.row * 2 + 1, render)),
                    None,
                    attrs,
                ),
                '█' => Span::styled("█", Some(block_ink(x, render)), None, attrs),
                other => Span::styled(other.to_string(), Some(block_ink(x, render)), None, attrs),
            }
        })
        .collect()
}

fn ink_at(x: usize, pixel_y: usize, render: RenderHalfAt<'_>) -> Rgba {
    let mix = idle_mix(x, pixel_y, render);
    tint(
        tint(render.ink, render.theme.primary, mix.1.clamp(0.0, 1.0)),
        PEAK,
        mix.0.clamp(0.0, 1.0),
    )
}

fn block_ink(x: usize, render: RenderHalfAt<'_>) -> Rgba {
    let top = idle_mix(x, render.row * 2, render);
    let bottom = idle_mix(x, render.row * 2 + 1, render);
    let mix = ((top.0 + bottom.0) * 0.5, (top.1 + bottom.1) * 0.5);
    tint(
        tint(render.ink, render.theme.primary, mix.1.clamp(0.0, 1.0)),
        PEAK,
        mix.0.clamp(0.0, 1.0),
    )
}

fn idle_mix(x: usize, pixel_y: usize, render: RenderHalfAt<'_>) -> (f32, f32) {
    let x = x as f32;
    let pixel_y = pixel_y as f32;
    let dx = x + 0.5 - SHIMMER_ORIGIN_X;
    let dy = pixel_y - SHIMMER_ORIGIN_Y;
    let dist = dx.hypot(dy);
    let angle = dy.atan2(dx);
    let wob1 = noise(x * 0.32, pixel_y * 0.25, render.elapsed_ms * 0.0005) - 0.5;
    let wob2 = noise(x * 0.12, pixel_y * 0.08, render.elapsed_ms * 0.00022) - 0.5;
    let ripple = (angle.mul_add(3.0, render.elapsed_ms * 0.0012)).sin() * 0.3;
    let traveled = dist + (wob1 * 0.55 + wob2 * 0.32 + ripple * 0.18) * SHIMMER_NOISE;

    let mut peak = 0.0;
    let mut primary = 0.0;
    let mut ambient = 0.0;
    for i in 0..SHIMMER_RINGS {
        let phase = (render.elapsed_ms / SHIMMER_PERIOD + i as f32 / SHIMMER_RINGS as f32).fract();
        let envelope = (phase * std::f32::consts::PI).sin();
        let eased = envelope * envelope * (3.0 - 2.0 * envelope);
        let delta = traveled - phase * render.reach;
        let core = (-(delta.abs() / SHIMMER_CORE_WIDTH).powf(1.8)).exp();
        let tail_range = SHIMMER_TAIL * 2.6;
        let tail = if delta < 0.0 && delta > -tail_range {
            (1.0 + delta / tail_range).powf(2.6)
        } else {
            0.0
        };
        let halo = (-((delta + SHIMMER_HALO_OFFSET).abs() / SHIMMER_HALO_WIDTH).powf(1.6)).exp();
        let d = (phase - SHIMMER_AMBIENT_CENTER) / SHIMMER_AMBIENT_WIDTH;

        peak += (core * SHIMMER_CORE_AMP + halo * SHIMMER_HALO_AMP) * eased;
        primary += (halo + tail * 0.6) * eased;
        ambient += if d.abs() < 1.0 {
            (1.0 - d * d).powi(2) * SHIMMER_AMBIENT_AMP
        } else {
            0.0
        };
    }

    let rings = SHIMMER_RINGS as f32;
    (
        SHIMMER_BREATH_BASE + ambient / rings + peak / rings,
        primary / rings * SHIMMER_PRIMARY_MIX,
    )
}

fn shimmer_reach() -> f32 {
    let width = f32::from(LOGO_WIDTH);
    let height = f32::from(LOGO_HEIGHT) * 2.0;
    [(0.0, 0.0), (width, 0.0), (0.0, height), (width, height)]
        .into_iter()
        .map(|(x, y)| (x - SHIMMER_ORIGIN_X).hypot(y - SHIMMER_ORIGIN_Y))
        .fold(0.0, f32::max)
        + SHIMMER_TAIL * 2.0
}

fn noise(x: f32, y: f32, t: f32) -> f32 {
    let n = (x.mul_add(12.9898, y.mul_add(78.233, t * 0.043))).sin() * 43_758.547;
    n - n.floor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin_theme, resolve, Mode};

    fn theme() -> ResolvedTheme {
        let json = builtin_theme("hya").unwrap().unwrap();
        resolve(&json, Mode::Dark).unwrap()
    }

    #[test]
    fn logo_has_four_rows_each_full_width() {
        let text = logo_text(&theme());
        assert_eq!(text.0.len(), 4);
        for line in &text.0 {
            assert_eq!(
                line.width(),
                LOGO_WIDTH as usize,
                "row width must be {LOGO_WIDTH}"
            );
        }
    }

    #[test]
    fn all_glyphs_use_muted_open_treatment_not_bold() {
        let theme = theme();
        let text = logo_text(&theme);
        for line in &text.0 {
            for span in &line.0 {
                assert!(!span.attrs.bold, "HYA logo is never bold");
            }
        }
        let row = &text.0[1];
        assert_eq!(row.0.first().unwrap().fg, Some(theme.text_muted));
        assert!(row.0.iter().any(|s| s.text.contains('█')));
    }

    #[test]
    fn shadow_marker_uses_tinted_shadow() {
        let theme = theme();
        let text = logo_text(&theme);
        // Row 2 left half "█__█" -> the "__" becomes spaces with a shadow bg.
        let shadow = tint(theme.background, theme.text_muted, SHADOW_ALPHA);
        let row = &text.0[2];
        let shadow_span = row
            .0
            .iter()
            .find(|s| s.bg == Some(shadow))
            .expect("underscore marker should carry a shadow background");
        assert_eq!(shadow_span.fg, Some(theme.text_muted));
    }

    #[test]
    fn logo_shimmer_varies_ink_color_over_time() {
        let theme = theme();
        let still = logo_text_at(&theme, std::time::Duration::ZERO);
        let later = logo_text_at(&theme, std::time::Duration::from_millis(600));

        let changed = still
            .0
            .iter()
            .zip(&later.0)
            .any(|(still_line, later_line)| {
                still_line
                    .0
                    .iter()
                    .zip(&later_line.0)
                    .any(|(still_span, later_span)| still_span.fg != later_span.fg)
            });

        assert!(changed, "idle shimmer should vary at least one ink color");
    }
}
