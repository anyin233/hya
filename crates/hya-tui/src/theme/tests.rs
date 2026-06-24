use crate::contracts::Rgba;

use super::{builtin_themes, resolve, Mode, DEFAULT_THEME};

#[test]
fn hya_default_golden_when_resolving_dark_and_light() {
    let themes = builtin_themes().expect("builtin themes parse");
    let theme = themes.get(DEFAULT_THEME).expect("default theme exists");

    let dark = resolve(theme, Mode::Dark).expect("dark theme resolves");
    let light = resolve(theme, Mode::Light).expect("light theme resolves");

    assert_eq!(dark.background, Rgba::rgb(0x0a, 0x0a, 0x0a));
    assert_eq!(dark.primary, Rgba::rgb(0xfa, 0xb2, 0x83));
    assert_eq!(dark.text, Rgba::rgb(0xee, 0xee, 0xee));
    assert_eq!(light.background, Rgba::rgb(0xff, 0xff, 0xff));
    assert_eq!(light.primary, Rgba::rgb(0x3b, 0x7d, 0xd8));
}

#[test]
fn alpha_blend_when_hex_has_alpha() {
    let foreground = Rgba::from_hex("#ffffff80").expect("valid alpha hex");
    let background = Rgba::rgb(0, 0, 0);

    let blended = foreground.over(background);

    assert_eq!(blended.a, 255);
    assert!((127..=129).contains(&blended.r));
    assert!((127..=129).contains(&blended.g));
    assert!((127..=129).contains(&blended.b));
}

#[test]
fn builtin_registry_when_resolving_all_modes() {
    let themes = builtin_themes().expect("builtin themes parse");

    assert_eq!(themes.len(), 33);
    for (name, theme) in &themes {
        resolve(theme, Mode::Dark).unwrap_or_else(|err| panic!("{name} dark failed: {err}"));
        resolve(theme, Mode::Light).unwrap_or_else(|err| panic!("{name} light failed: {err}"));
    }
}
