// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Liquid Glass design system — translucent, layered surfaces inspired by Apple's design language.
//!
//! Provides a unified style vocabulary for the Ryve UI across dark and light modes.

use iced::widget::{button, container};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

/// System appearance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

impl Appearance {
    /// Detect the system appearance (dark or light mode).
    pub fn detect() -> Self {
        match dark_light::detect() {
            Ok(dark_light::Mode::Light) => Self::Light,
            _ => Self::Dark,
        }
    }

    /// Return the iced theme corresponding to this appearance.
    pub fn theme(&self) -> iced::Theme {
        match self {
            Self::Dark => iced::Theme::Dark,
            Self::Light => iced::Theme::Light,
        }
    }

    /// Get the color palette for this appearance.
    pub fn palette(&self) -> Palette {
        match self {
            Self::Dark => Palette::dark(),
            Self::Light => Palette::light(),
        }
    }
}

// ── Color Palette ────────────────────────────────────

/// Color palette for the liquid glass design system.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window/app background color.
    pub window_bg: Color,
    /// Glass surface — subtle translucent fill.
    pub surface: Color,
    /// Glass surface on hover.
    pub surface_hover: Color,
    /// Glass surface when active/selected.
    pub surface_active: Color,
    /// Default border color.
    pub border: Color,
    /// Border color for active/focused elements.
    pub border_active: Color,
    /// Primary text color.
    pub text_primary: Color,
    /// Secondary/muted text color.
    pub text_secondary: Color,
    /// Tertiary/disabled text color.
    pub text_tertiary: Color,
    /// Accent color (system blue).
    pub accent: Color,
    /// Dimmed accent for subtle highlights.
    pub accent_dim: Color,
    /// Separator lines between sections.
    pub separator: Color,
    /// Danger/destructive action color.
    pub danger: Color,
    /// Success/idle indicator color (green).
    pub success: Color,
    /// Tab background (inactive).
    pub tab_bg: Color,
    /// Tab background (active).
    pub tab_active: Color,
    /// Modal overlay backdrop.
    pub overlay: Color,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            window_bg: Color {
                r: 0.110,
                g: 0.110,
                b: 0.118,
                a: 1.0,
            },
            surface: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.05,
            },
            surface_hover: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.08,
            },
            surface_active: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.12,
            },
            border: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.08,
            },
            border_active: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.18,
            },
            text_primary: Color {
                r: 0.949,
                g: 0.949,
                b: 0.969,
                a: 1.0,
            },
            text_secondary: Color {
                r: 0.557,
                g: 0.557,
                b: 0.576,
                a: 1.0,
            },
            text_tertiary: Color {
                r: 0.388,
                g: 0.388,
                b: 0.400,
                a: 1.0,
            },
            accent: Color {
                r: 0.039,
                g: 0.518,
                b: 1.0,
                a: 1.0,
            },
            accent_dim: Color {
                r: 0.039,
                g: 0.518,
                b: 1.0,
                a: 0.15,
            },
            separator: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.06,
            },
            danger: Color {
                r: 1.0,
                g: 0.271,
                b: 0.227,
                a: 1.0,
            },
            success: Color {
                r: 0.196,
                g: 0.843,
                b: 0.294,
                a: 1.0,
            },
            tab_bg: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.04,
            },
            tab_active: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.12,
            },
            overlay: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.5,
            },
        }
    }

    pub fn light() -> Self {
        Self {
            window_bg: Color {
                r: 0.949,
                g: 0.949,
                b: 0.969,
                a: 1.0,
            },
            surface: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.70,
            },
            surface_hover: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.80,
            },
            surface_active: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.90,
            },
            border: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.08,
            },
            border_active: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.15,
            },
            text_primary: Color::BLACK,
            text_secondary: Color {
                r: 0.557,
                g: 0.557,
                b: 0.576,
                a: 1.0,
            },
            text_tertiary: Color {
                r: 0.682,
                g: 0.682,
                b: 0.698,
                a: 1.0,
            },
            accent: Color {
                r: 0.0,
                g: 0.478,
                b: 1.0,
                a: 1.0,
            },
            accent_dim: Color {
                r: 0.0,
                g: 0.478,
                b: 1.0,
                a: 0.12,
            },
            separator: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.08,
            },
            danger: Color {
                r: 1.0,
                g: 0.231,
                b: 0.188,
                a: 1.0,
            },
            success: Color {
                r: 0.204,
                g: 0.780,
                b: 0.349,
                a: 1.0,
            },
            tab_bg: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.04,
            },
            tab_active: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.08,
            },
            overlay: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.35,
            },
        }
    }
}

// ── Style Builders ───────────────────────────────────

/// Standard glass panel (sidebar, bench, sparks).
///
/// When a background image is present, the panel gets a translucent
/// "liquid glass" fill so the image bleeds through — like looking
/// through frosted glass.  Without a background the panel is a
/// simple subtle surface.
pub fn glass_panel(pal: &Palette, has_bg: bool) -> container::Style {
    if has_bg {
        container::Style {
            background: Some(Background::Color(Color {
                r: pal.window_bg.r,
                g: pal.window_bg.g,
                b: pal.window_bg.b,
                a: 0.55,
            })),
            border: Border {
                color: Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 0.10,
                },
                width: 1.0,
                radius: 10.0.into(),
            },
            shadow: Shadow {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.18,
                },
                offset: Vector::new(0.0, 2.0),
                blur_radius: 12.0,
            },
            ..Default::default()
        }
    } else {
        container::Style {
            background: Some(Background::Color(pal.surface)),
            border: Border {
                color: pal.border,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        }
    }
}

/// Workshop/global tab bar strip.
pub fn tab_bar(pal: &Palette, has_bg: bool) -> container::Style {
    container::Style {
        background: if has_bg {
            None
        } else {
            Some(Background::Color(pal.surface))
        },
        border: Border {
            color: pal.separator,
            width: 0.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

/// Individual tab pill — active or inactive.
pub fn tab_pill(pal: &Palette, active: bool) -> container::Style {
    if active {
        container::Style {
            background: Some(Background::Color(pal.tab_active)),
            border: Border {
                color: pal.border_active,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }
    } else {
        container::Style {
            background: Some(Background::Color(pal.tab_bg)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }
    }
}

/// Status bar at the bottom.
pub fn status_bar_style(pal: &Palette, has_bg: bool) -> container::Style {
    container::Style {
        background: if has_bg {
            // Slightly more opaque for readability over background images
            Some(Background::Color(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.45,
            }))
        } else {
            Some(Background::Color(pal.surface))
        },
        border: Border {
            color: pal.separator,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

/// Dropdown menu container.
pub fn dropdown(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.95,
            ..pal.window_bg
        })),
        border: Border {
            color: pal.border_active,
            width: 1.0,
            radius: 8.0.into(),
        },
        shadow: Shadow {
            color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.25,
            },
            offset: Vector::new(0.0, 4.0),
            blur_radius: 16.0,
        },
        ..Default::default()
    }
}

/// Modal dialog container.
pub fn modal(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.95,
            ..pal.window_bg
        })),
        border: Border {
            color: pal.border_active,
            width: 1.0,
            radius: 12.0.into(),
        },
        shadow: Shadow {
            color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.3,
            },
            offset: Vector::new(0.0, 8.0),
            blur_radius: 32.0,
        },
        ..Default::default()
    }
}

/// Modal backdrop overlay.
pub fn modal_backdrop(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(Background::Color(pal.overlay)),
        ..Default::default()
    }
}

/// Selected file/item highlight.
pub fn selected_item(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(Background::Color(pal.surface_active)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

/// Hovered item highlight.
pub fn hovered_item(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(Background::Color(pal.surface_hover)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

/// Button style for list rows (file explorer, etc.): transparent when idle,
/// painted with [`hovered_item`] on hover. Text color defaults to the palette
/// primary so inherited glyphs (e.g. the spark-link icon) stay legible in both
/// light and dark modes; inner `text` widgets that set an explicit color are
/// unaffected.
pub fn row_button(pal: Palette) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let base = button::Style {
            text_color: pal.text_primary,
            ..button::Style::default()
        };
        match status {
            button::Status::Hovered => {
                let hov = hovered_item(&pal);
                button::Style {
                    background: hov.background,
                    border: hov.border,
                    ..base
                }
            }
            _ => base,
        }
    }
}

/// Danger/destructive action container.
#[allow(dead_code)]
pub fn danger_surface(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.1,
            ..pal.danger
        })),
        border: Border {
            color: pal.danger,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

// ── Layout Constants ─────────────────────────────────

// ── Font Size Scale ──────────────────────────────────

/// Panel/section headers (e.g. "Workgraph", "Files", "Hands")
pub const FONT_HEADER: f32 = 16.0;
/// Primary body text (file names, spark titles, session names)
pub const FONT_BODY: f32 = 14.0;
/// Secondary labels (priority badges, branch names, timestamps)
pub const FONT_LABEL: f32 = 12.0;
/// Small supplementary text (section dividers, hints)
pub const FONT_SMALL: f32 = 11.0;
/// Icon size in panels (status indicators, action buttons)
pub const FONT_ICON: f32 = 14.0;
/// Small icon size (inline indicators, badges)
pub const FONT_ICON_SM: f32 = 12.0;

// ── Layout Constants ─────────────────────────────────

/// Gap between major panels (sidebar ↔ bench ↔ sparks).
pub const PANEL_GAP: f32 = 6.0;

/// Height reserved for macOS title bar (traffic lights).
#[cfg(target_os = "macos")]
pub const TITLE_BAR_TOP_PAD: f32 = 4.0;

#[cfg(not(target_os = "macos"))]
pub const TITLE_BAR_TOP_PAD: f32 = 0.0;

/// Left padding to clear macOS traffic lights.
#[cfg(target_os = "macos")]
pub const TRAFFIC_LIGHT_WIDTH: f32 = 72.0;

#[cfg(not(target_os = "macos"))]
pub const TRAFFIC_LIGHT_WIDTH: f32 = 0.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// `row_button` must be transparent when idle and show the `hovered_item`
    /// background when hovered — this is the wiring that gives file explorer
    /// rows their hover feedback (sp-ux0024).
    #[test]
    fn row_button_highlights_on_hover() {
        let pal = Palette::dark();
        let style_fn = row_button(pal);
        let theme = Theme::Dark;

        let active = style_fn(&theme, button::Status::Active);
        assert!(
            active.background.is_none(),
            "row button should have no background when idle"
        );

        let hovered = style_fn(&theme, button::Status::Hovered);
        let expected = hovered_item(&pal).background;
        assert_eq!(
            hovered.background, expected,
            "hovered row must reuse hovered_item's background"
        );
    }
}
