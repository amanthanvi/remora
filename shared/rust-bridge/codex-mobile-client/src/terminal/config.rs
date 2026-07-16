//! Terminal config: theme presets, font, cursor style, `ghostty.conf` rendering.
//!
//! Renders to a ghostty.conf file the platform feeds to `ghostty_config_new` +
//! `ghostty_config_load_file` + `ghostty_*_update_config`. Keeping the
//! palette/preset table in Rust lets iOS + Android show the same theme list
//! without duplicating colour values.

use std::fmt::Write;

/// User-facing terminal configuration. Persisted by each platform's
/// preferences store and re-applied on launch.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TerminalConfig {
    pub theme: TerminalThemePreset,
    pub font_family: String,
    /// Point size; clamped to [8.0, 32.0] by `render_ghostty_conf`.
    pub font_size_pt: f32,
    pub cursor_style: TerminalCursorStyle,
    pub cursor_blink: bool,
    /// Lines of scrollback. Clamped to [100, 100_000].
    pub scrollback_lines: u32,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum TerminalThemePreset {
    RemoraDark,
    CatppuccinFrappe,
    CatppuccinFrappeLight,
    Solarized { dark: bool },
    Custom { ghostty_conf: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalCursorStyle {
    Block,
    Bar,
    Underscore,
}

/// 16 ANSI palette colours plus foreground / background / cursor. Each entry
/// is a `#rrggbb` hex string. Exposed via UniFFI so platform UI (e.g. a
/// theme-picker preview swatch) can read the same values without parsing
/// ghostty.conf.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TerminalPalette {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    /// ANSI 0..7 (regular).
    pub ansi: Vec<String>,
    /// ANSI 8..15 (bright).
    pub bright: Vec<String>,
}

/// Render `config` as the contents of a `ghostty.conf` file. The caller is
/// expected to write this to a path and hand the path to
/// `ghostty_config_load_file` via the backend.
#[uniffi::export]
pub fn render_ghostty_conf(config: TerminalConfig) -> String {
    let font_size = config.font_size_pt.clamp(8.0, 32.0);
    let scrollback = config.scrollback_lines.clamp(100, 100_000);

    let mut out = String::with_capacity(1024);
    let _ = writeln!(out, "# remora-mobile generated ghostty config");
    let _ = writeln!(out);

    if !config.font_family.trim().is_empty() {
        let _ = writeln!(out, "font-family = {}", config.font_family.trim());
    }
    let _ = writeln!(out, "font-size = {}", format_font_size(font_size));

    let cursor_keyword = match config.cursor_style {
        TerminalCursorStyle::Block => "block",
        TerminalCursorStyle::Bar => "bar",
        TerminalCursorStyle::Underscore => "underline",
    };
    let _ = writeln!(out, "cursor-style = {cursor_keyword}");
    let _ = writeln!(
        out,
        "cursor-style-blink = {}",
        if config.cursor_blink { "true" } else { "false" }
    );
    let _ = writeln!(out, "scrollback-limit = {scrollback}");

    let palette = theme_palette(config.theme.clone());
    let _ = writeln!(out);
    let _ = writeln!(out, "background = {}", palette.background);
    let _ = writeln!(out, "foreground = {}", palette.foreground);
    let _ = writeln!(out, "cursor-color = {}", palette.cursor);
    for (i, color) in palette.ansi.iter().enumerate() {
        let _ = writeln!(out, "palette = {i}={color}");
    }
    for (i, color) in palette.bright.iter().enumerate() {
        let _ = writeln!(out, "palette = {}={}", i + 8, color);
    }

    if let TerminalThemePreset::Custom { ghostty_conf } = config.theme {
        let trimmed = ghostty_conf.trim();
        if !trimmed.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "# --- custom ghostty.conf overrides ---");
            let _ = writeln!(out, "{trimmed}");
        }
    }

    out
}

fn format_font_size(value: f32) -> String {
    // Print whole numbers without trailing `.0` so the rendered conf stays
    // deterministic and snapshot-friendly: `13` rather than `13.0`.
    if (value - value.round()).abs() < f32::EPSILON {
        format!("{}", value.round() as i32)
    } else {
        format!("{value:.1}")
    }
}

/// Look up the 16 ANSI colours + fg/bg/cursor for a preset. Custom presets
/// fall back to [`TerminalThemePreset::RemoraDark`] for the base palette
/// (the `Custom.ghostty_conf` text is appended verbatim and may override).
#[uniffi::export]
pub fn theme_palette(preset: TerminalThemePreset) -> TerminalPalette {
    match preset {
        TerminalThemePreset::RemoraDark => remora_dark_palette(),
        TerminalThemePreset::CatppuccinFrappe => catppuccin_frappe_palette(false),
        TerminalThemePreset::CatppuccinFrappeLight => catppuccin_frappe_palette(true),
        TerminalThemePreset::Solarized { dark } => solarized_palette(dark),
        TerminalThemePreset::Custom { .. } => remora_dark_palette(),
    }
}

fn remora_dark_palette() -> TerminalPalette {
    TerminalPalette {
        background: "#02082C".into(),
        foreground: "#EAFBFF".into(),
        cursor: "#0DD5F0".into(),
        ansi: vec![
            "#000000".into(),
            "#FF5C57".into(),
            "#00FF9C".into(),
            "#F3F99D".into(),
            "#57C7FF".into(),
            "#FF6AC1".into(),
            "#9AEDFE".into(),
            "#C0C5CE".into(),
        ],
        bright: vec![
            "#686868".into(),
            "#FF6E67".into(),
            "#5AF78E".into(),
            "#F4F99D".into(),
            "#62D6FF".into(),
            "#FF8AC8".into(),
            "#9AEDFE".into(),
            "#FFFFFF".into(),
        ],
    }
}

fn catppuccin_frappe_palette(light: bool) -> TerminalPalette {
    let (background, foreground) = if light {
        ("#EFF1F5", "#4C4F69")
    } else {
        ("#303446", "#C6D0F5")
    };
    TerminalPalette {
        background: background.into(),
        foreground: foreground.into(),
        cursor: foreground.into(),
        ansi: vec![
            "#51576D".into(),
            "#E78284".into(),
            "#A6D189".into(),
            "#E5C890".into(),
            "#8CAAEE".into(),
            "#F4B8E4".into(),
            "#81C8BE".into(),
            "#B5BFE2".into(),
        ],
        bright: vec![
            "#626880".into(),
            "#E78284".into(),
            "#A6D189".into(),
            "#E5C890".into(),
            "#8CAAEE".into(),
            "#F4B8E4".into(),
            "#81C8BE".into(),
            "#A5ADCE".into(),
        ],
    }
}

fn solarized_palette(dark: bool) -> TerminalPalette {
    let (background, foreground) = if dark {
        ("#002B36", "#839496")
    } else {
        ("#FDF6E3", "#657B83")
    };
    TerminalPalette {
        background: background.into(),
        foreground: foreground.into(),
        cursor: foreground.into(),
        ansi: vec![
            "#073642".into(),
            "#DC322F".into(),
            "#859900".into(),
            "#B58900".into(),
            "#268BD2".into(),
            "#D33682".into(),
            "#2AA198".into(),
            "#EEE8D5".into(),
        ],
        bright: vec![
            "#002B36".into(),
            "#CB4B16".into(),
            "#586E75".into(),
            "#657B83".into(),
            "#839496".into(),
            "#6C71C4".into(),
            "#93A1A1".into(),
            "#FDF6E3".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(hex: &str) -> [f64; 3] {
        let rgb = hex.strip_prefix('#').expect("palette colors use #RRGGBB");
        assert_eq!(rgb.len(), 6);
        [0, 2, 4]
            .map(|offset| u8::from_str_radix(&rgb[offset..offset + 2], 16).unwrap() as f64 / 255.0)
    }

    fn relative_luminance_rgb(rgb: [f64; 3]) -> f64 {
        let channel = |value: f64| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
    }

    fn relative_luminance(hex: &str) -> f64 {
        relative_luminance_rgb(rgb(hex))
    }

    fn contrast_ratio(foreground: &str, background: &str) -> f64 {
        let foreground = relative_luminance(foreground);
        let background = relative_luminance(background);
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

    fn contrast_ratio_on_tinted_background(
        foreground: &str,
        background: &str,
        tint_alpha: f64,
    ) -> f64 {
        let foreground_rgb = rgb(foreground);
        let background_rgb = rgb(background);
        let tinted_background = [0, 1, 2].map(|index| {
            foreground_rgb[index] * tint_alpha + background_rgb[index] * (1.0 - tint_alpha)
        });
        let foreground = relative_luminance_rgb(foreground_rgb);
        let background = relative_luminance_rgb(tinted_background);
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

    fn cfg() -> TerminalConfig {
        TerminalConfig {
            theme: TerminalThemePreset::RemoraDark,
            font_family: "SFMono-Regular".into(),
            font_size_pt: 13.0,
            cursor_style: TerminalCursorStyle::Bar,
            cursor_blink: true,
            scrollback_lines: 10_000,
        }
    }

    #[test]
    fn renders_remora_dark_with_palette_lines() {
        let out = render_ghostty_conf(cfg());
        assert!(out.contains("font-family = SFMono-Regular"));
        assert!(out.contains("font-size = 13"));
        assert!(out.contains("cursor-style = bar"));
        assert!(out.contains("cursor-style-blink = true"));
        assert!(out.contains("scrollback-limit = 10000"));
        assert!(out.contains("background = #02082C"));
        assert!(out.contains("foreground = #EAFBFF"));
        assert!(out.contains("cursor-color = #0DD5F0"));
        assert!(out.contains("palette = 0=#000000"));
        assert!(out.contains("palette = 2=#00FF9C"));
        assert!(out.contains("palette = 10=#5AF78E"));
        assert!(out.contains("palette = 15=#FFFFFF"));
    }

    #[test]
    fn remora_dark_uses_ocean_identity_without_repurposing_ansi_green() {
        let palette = theme_palette(TerminalThemePreset::RemoraDark);

        assert_eq!(palette.background, "#02082C");
        assert_eq!(palette.foreground, "#EAFBFF");
        assert_eq!(palette.cursor, "#0DD5F0");
        assert_eq!(palette.ansi[2], "#00FF9C");
        assert_eq!(palette.bright[2], "#5AF78E");
    }

    #[test]
    fn platform_status_foregrounds_meet_wcag_aa_on_every_surface() {
        let cases = [
            ("remora-dark", TerminalThemePreset::RemoraDark, false),
            (
                "catppuccin-frappe",
                TerminalThemePreset::CatppuccinFrappe,
                false,
            ),
            (
                "catppuccin-frappe-light",
                TerminalThemePreset::CatppuccinFrappeLight,
                true,
            ),
            (
                "solarized-dark",
                TerminalThemePreset::Solarized { dark: true },
                false,
            ),
            (
                "solarized-light",
                TerminalThemePreset::Solarized { dark: false },
                true,
            ),
        ];

        for (name, preset, uses_ansi_black) in cases {
            let palette = theme_palette(preset);
            let foreground = if uses_ansi_black {
                palette.ansi.first().unwrap_or(&palette.foreground)
            } else {
                &palette.foreground
            };
            let ratio = contrast_ratio(foreground, &palette.background);
            assert!(
                ratio >= 4.5,
                "{name} status foreground {foreground} on {} has contrast {ratio:.2}",
                palette.background
            );
        }

        let chrome = theme_palette(TerminalThemePreset::RemoraDark);
        let chip_roles = [
            ("neutral", chrome.foreground.as_str()),
            ("running", chrome.cursor.as_str()),
            ("failed", chrome.ansi[1].as_str()),
        ];
        for (name, foreground) in chip_roles {
            let ratio = contrast_ratio_on_tinted_background(foreground, &chrome.background, 0.12);
            assert!(
                ratio >= 4.5,
                "{name} phase-chip foreground {foreground} on tinted {} has contrast {ratio:.2}",
                chrome.background
            );
        }
    }

    #[test]
    fn clamps_font_size_and_scrollback() {
        let mut c = cfg();
        c.font_size_pt = 200.0;
        c.scrollback_lines = 5;
        let out = render_ghostty_conf(c);
        assert!(out.contains("font-size = 32"));
        assert!(out.contains("scrollback-limit = 100"));

        let mut c = cfg();
        c.font_size_pt = 2.0;
        c.scrollback_lines = 10_000_000;
        let out = render_ghostty_conf(c);
        assert!(out.contains("font-size = 8"));
        assert!(out.contains("scrollback-limit = 100000"));
    }

    #[test]
    fn fractional_font_size_uses_one_decimal() {
        let mut c = cfg();
        c.font_size_pt = 13.5;
        let out = render_ghostty_conf(c);
        assert!(out.contains("font-size = 13.5"));
    }

    #[test]
    fn custom_theme_appends_user_conf_after_base_palette() {
        let mut c = cfg();
        c.theme = TerminalThemePreset::Custom {
            ghostty_conf: "background = #112233\nfont-feature = +liga".into(),
        };
        let out = render_ghostty_conf(c);
        // Base palette still rendered (from RemoraDark fallback).
        assert!(out.contains("foreground = #EAFBFF"));
        // Custom block follows.
        let base = out.find("# --- custom ghostty.conf overrides ---").unwrap();
        let custom = out.find("font-feature = +liga").unwrap();
        assert!(custom > base);
    }

    #[test]
    fn solarized_light_and_dark_have_distinct_backgrounds() {
        let dark = theme_palette(TerminalThemePreset::Solarized { dark: true });
        let light = theme_palette(TerminalThemePreset::Solarized { dark: false });
        assert_ne!(dark.background, light.background);
        assert_eq!(dark.ansi.len(), 8);
        assert_eq!(dark.bright.len(), 8);
    }

    #[test]
    fn cursor_styles_render_distinct_keywords() {
        let mut c = cfg();
        c.cursor_style = TerminalCursorStyle::Block;
        assert!(render_ghostty_conf(c.clone()).contains("cursor-style = block"));
        c.cursor_style = TerminalCursorStyle::Underscore;
        assert!(render_ghostty_conf(c).contains("cursor-style = underline"));
    }
}
