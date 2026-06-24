use std::collections::BTreeMap;

use crate::contracts::Rgba;

use super::error::ThemeError;
use super::resolved::{
    BACKGROUND_ELEMENT_KEY, BACKGROUND_KEY, BACKGROUND_MENU_KEY, PRIMARY_KEY,
    SELECTED_LIST_ITEM_TEXT_KEY, THEME_COLOR_KEYS, THINKING_OPACITY_KEY,
};
use super::schema::{Mode, ThemeJson, ThemeValue};
use super::ResolvedTheme;

const DEFAULT_THINKING_OPACITY: f64 = 0.6;

pub fn resolve(theme: &ThemeJson, mode: Mode) -> Result<ResolvedTheme, ThemeError> {
    let mut resolver = ColorResolver::new(theme, mode);
    let mut colors = BTreeMap::new();
    for &key in THEME_COLOR_KEYS {
        if matches!(key, SELECTED_LIST_ITEM_TEXT_KEY | BACKGROUND_MENU_KEY) {
            continue;
        }
        colors.insert(key, resolver.resolve_theme_key(key)?);
    }

    let has_selected_list_item_text = theme.theme.contains_key(SELECTED_LIST_ITEM_TEXT_KEY);
    let selected_list_item_text = if has_selected_list_item_text {
        resolver.resolve_theme_key(SELECTED_LIST_ITEM_TEXT_KEY)?
    } else {
        fallback_selected_list_item_text(&colors)?
    };
    colors.insert(SELECTED_LIST_ITEM_TEXT_KEY, selected_list_item_text);

    let background_menu = if theme.theme.contains_key(BACKGROUND_MENU_KEY) {
        resolver.resolve_theme_key(BACKGROUND_MENU_KEY)?
    } else {
        required_color(&colors, BACKGROUND_ELEMENT_KEY)?
    };
    colors.insert(BACKGROUND_MENU_KEY, background_menu);

    ResolvedTheme::try_from_colors(
        |key| required_color(&colors, key),
        thinking_opacity(theme)?,
        has_selected_list_item_text,
    )
}

pub fn selected_foreground(theme: &ResolvedTheme, background: Option<Rgba>) -> Rgba {
    if theme.has_selected_list_item_text {
        return theme.selected_list_item_text;
    }
    if theme.background.a == 0 {
        return contrast_on(background.unwrap_or(theme.primary));
    }
    theme.background
}

pub fn ansi_to_rgba(code: u8) -> Rgba {
    match code {
        0 => Rgba::rgb(0x00, 0x00, 0x00),
        1 => Rgba::rgb(0x80, 0x00, 0x00),
        2 => Rgba::rgb(0x00, 0x80, 0x00),
        3 => Rgba::rgb(0x80, 0x80, 0x00),
        4 => Rgba::rgb(0x00, 0x00, 0x80),
        5 => Rgba::rgb(0x80, 0x00, 0x80),
        6 => Rgba::rgb(0x00, 0x80, 0x80),
        7 => Rgba::rgb(0xc0, 0xc0, 0xc0),
        8 => Rgba::rgb(0x80, 0x80, 0x80),
        9 => Rgba::rgb(0xff, 0x00, 0x00),
        10 => Rgba::rgb(0x00, 0xff, 0x00),
        11 => Rgba::rgb(0xff, 0xff, 0x00),
        12 => Rgba::rgb(0x00, 0x00, 0xff),
        13 => Rgba::rgb(0xff, 0x00, 0xff),
        14 => Rgba::rgb(0x00, 0xff, 0xff),
        15 => Rgba::rgb(0xff, 0xff, 0xff),
        16..=231 => {
            let index = code - 16;
            Rgba::rgb(
                ansi_cube_channel(index / 36),
                ansi_cube_channel((index / 6) % 6),
                ansi_cube_channel(index % 6),
            )
        }
        232..=255 => {
            let gray = (code - 232) * 10 + 8;
            Rgba::rgb(gray, gray, gray)
        }
    }
}

pub fn tint(base: Rgba, overlay: Rgba, alpha: f32) -> Rgba {
    Rgba::rgb(
        tint_channel(base.r, overlay.r, alpha),
        tint_channel(base.g, overlay.g, alpha),
        tint_channel(base.b, overlay.b, alpha),
    )
}

struct ColorResolver<'a> {
    theme: &'a ThemeJson,
    mode: Mode,
    chain: Vec<String>,
}

impl<'a> ColorResolver<'a> {
    fn new(theme: &'a ThemeJson, mode: Mode) -> Self {
        Self {
            theme,
            mode,
            chain: Vec::new(),
        }
    }

    fn resolve_theme_key(&mut self, key: &'static str) -> Result<Rgba, ThemeError> {
        let value = self
            .theme
            .theme
            .get(key)
            .ok_or(ThemeError::MissingThemeKey { key })?
            .clone();
        self.resolve_value(&value)
    }

    fn resolve_value(&mut self, value: &ThemeValue) -> Result<Rgba, ThemeError> {
        match value {
            ThemeValue::Variant(variant) => self.resolve_value(variant.get(self.mode)),
            ThemeValue::String(text) => self.resolve_string(text),
            ThemeValue::Number(number) => number
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .map(ansi_to_rgba)
                .ok_or_else(|| ThemeError::InvalidAnsi {
                    value: number.to_string(),
                }),
        }
    }

    fn resolve_string(&mut self, text: &str) -> Result<Rgba, ThemeError> {
        if text.eq_ignore_ascii_case("transparent") || text.eq_ignore_ascii_case("none") {
            return Ok(Rgba::TRANSPARENT);
        }
        if text.starts_with('#') {
            return Rgba::from_hex(text).ok_or_else(|| ThemeError::InvalidHex {
                value: text.to_owned(),
            });
        }
        self.resolve_reference(text)
    }

    fn resolve_reference(&mut self, name: &str) -> Result<Rgba, ThemeError> {
        if self.chain.iter().any(|item| item == name) {
            let mut chain = self.chain.clone();
            chain.push(name.to_owned());
            return Err(ThemeError::CircularReference {
                chain: chain.join(" -> "),
            });
        }
        let value = self
            .theme
            .defs
            .get(name)
            .map(|text| ThemeValue::String(text.clone()))
            .or_else(|| self.theme.theme.get(name).cloned())
            .ok_or_else(|| ThemeError::MissingReference {
                name: name.to_owned(),
            })?;
        self.chain.push(name.to_owned());
        let resolved = self.resolve_value(&value);
        self.chain.pop();
        resolved
    }
}

fn fallback_selected_list_item_text(
    colors: &BTreeMap<&'static str, Rgba>,
) -> Result<Rgba, ThemeError> {
    let background = required_color(colors, BACKGROUND_KEY)?;
    if background.a == 0 {
        return Ok(contrast_on(required_color(colors, PRIMARY_KEY)?));
    }
    Ok(background)
}

fn required_color(
    colors: &BTreeMap<&'static str, Rgba>,
    key: &'static str,
) -> Result<Rgba, ThemeError> {
    colors
        .get(key)
        .copied()
        .ok_or(ThemeError::MissingThemeKey { key })
}

fn thinking_opacity(theme: &ThemeJson) -> Result<f64, ThemeError> {
    match theme.theme.get(THINKING_OPACITY_KEY) {
        None => Ok(DEFAULT_THINKING_OPACITY),
        Some(ThemeValue::Number(number)) => number.as_f64().ok_or(ThemeError::InvalidNumber {
            key: THINKING_OPACITY_KEY,
        }),
        Some(ThemeValue::String(_) | ThemeValue::Variant(_)) => Err(ThemeError::InvalidNumber {
            key: THINKING_OPACITY_KEY,
        }),
    }
}

fn contrast_on(color: Rgba) -> Rgba {
    if luminance(color) > 127.5 {
        Rgba::rgb(0, 0, 0)
    } else {
        Rgba::rgb(255, 255, 255)
    }
}

fn luminance(color: Rgba) -> f64 {
    0.299_f64.mul_add(
        f64::from(color.r),
        0.587_f64.mul_add(f64::from(color.g), 0.114 * f64::from(color.b)),
    )
}

fn ansi_cube_channel(value: u8) -> u8 {
    if value == 0 {
        0
    } else {
        value * 40 + 55
    }
}

fn tint_channel(base: u8, overlay: u8, alpha: f32) -> u8 {
    f32::from(base)
        .mul_add(1.0 - alpha, f32::from(overlay) * alpha)
        .round() as u8
}
