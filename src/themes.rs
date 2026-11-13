/// Color-scheme themes for the whole app (chrome + reader text).
/// Palettes follow the official specs of each scheme (see per-variant notes).
/// `Default Dark` / `Default Light` are egui's built-in visuals, unchanged.
/// A UI palette: all built-in widget states derive from these nine colors.
#[derive(Debug, Clone, Copy)]
struct Palette {
    bg: egui::Color32,
    surface: egui::Color32,
    fg: egui::Color32,
    muted: egui::Color32,
    accent: egui::Color32,
    selection_bg: egui::Color32,
    selection_stroke: egui::Color32,
    warn: egui::Color32,
    error: egui::Color32,
}

const fn c(hex: u32) -> egui::Color32 {
    egui::Color32::from_rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeId {
    DefaultDark,
    DefaultLight,
    GruvboxDark,
    GruvboxLight,
    SolarizedDark,
    SolarizedLight,
    Dracula,
    NordDark,
    NordLight,
    CatppuccinMocha,
    CatppuccinFrappe,
    CatppuccinMacchiato,
    CatppuccinLatte,
    EverforestDark,
    EverforestLight,
    RosePine,
    RosePineMoon,
    RosePineDawn,
}

impl ThemeId {
    pub fn id(self) -> &'static str {
        match self {
            ThemeId::DefaultDark => "default-dark",
            ThemeId::DefaultLight => "default-light",
            ThemeId::GruvboxDark => "gruvbox-dark",
            ThemeId::GruvboxLight => "gruvbox-light",
            ThemeId::SolarizedDark => "solarized-dark",
            ThemeId::SolarizedLight => "solarized-light",
            ThemeId::Dracula => "dracula",
            ThemeId::NordDark => "nord-dark",
            ThemeId::NordLight => "nord-light",
            ThemeId::CatppuccinMocha => "catppuccin-mocha",
            ThemeId::CatppuccinFrappe => "catppuccin-frappe",
            ThemeId::CatppuccinMacchiato => "catppuccin-macchiato",
            ThemeId::CatppuccinLatte => "catppuccin-latte",
            ThemeId::EverforestDark => "everforest-dark",
            ThemeId::EverforestLight => "everforest-light",
            ThemeId::RosePine => "rose-pine",
            ThemeId::RosePineMoon => "rose-pine-moon",
            ThemeId::RosePineDawn => "rose-pine-dawn",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ThemeId::DefaultDark => "Default Dark",
            ThemeId::DefaultLight => "Default Light",
            ThemeId::GruvboxDark => "Gruvbox Dark",
            ThemeId::GruvboxLight => "Gruvbox Light",
            ThemeId::SolarizedDark => "Solarized Dark",
            ThemeId::SolarizedLight => "Solarized Light",
            ThemeId::Dracula => "Dracula",
            ThemeId::NordDark => "Nord Dark",
            ThemeId::NordLight => "Nord Light",
            ThemeId::CatppuccinMocha => "Catppuccin Mocha",
            ThemeId::CatppuccinFrappe => "Catppuccin Frappé",
            ThemeId::CatppuccinMacchiato => "Catppuccin Macchiato",
            ThemeId::CatppuccinLatte => "Catppuccin Latte",
            ThemeId::EverforestDark => "Everforest Dark",
            ThemeId::EverforestLight => "Everforest Light",
            ThemeId::RosePine => "Rosé Pine",
            ThemeId::RosePineMoon => "Rosé Pine Moon",
            ThemeId::RosePineDawn => "Rosé Pine Dawn",
        }
    }

    pub fn is_dark(self) -> bool {
        !matches!(
            self,
            ThemeId::DefaultLight
                | ThemeId::GruvboxLight
                | ThemeId::SolarizedLight
                | ThemeId::NordLight
                | ThemeId::CatppuccinLatte
                | ThemeId::EverforestLight
                | ThemeId::RosePineDawn
        )
    }

    pub fn all() -> &'static [ThemeId] {
        use ThemeId::*;
        &[
            DefaultDark,
            DefaultLight,
            GruvboxDark,
            GruvboxLight,
            SolarizedDark,
            SolarizedLight,
            Dracula,
            NordDark,
            NordLight,
            CatppuccinMocha,
            CatppuccinFrappe,
            CatppuccinMacchiato,
            CatppuccinLatte,
            EverforestDark,
            EverforestLight,
            RosePine,
            RosePineMoon,
            RosePineDawn,
        ]
    }

    pub fn all_light() -> impl Iterator<Item = ThemeId> {
        Self::all().iter().copied().filter(|t| !t.is_dark())
    }

    pub fn all_dark() -> impl Iterator<Item = ThemeId> {
        Self::all().iter().copied().filter(|t| t.is_dark())
    }

    pub fn from_id(id: &str) -> Option<ThemeId> {
        Self::all().iter().copied().find(|t| t.id() == id)
    }

    fn palette(self) -> Option<Palette> {
        match self {
            ThemeId::DefaultDark | ThemeId::DefaultLight => None,
            // https://github.com/morhetz/gruvbox — bg/bg1/bg2, fg, gray,
            // bright blue accent; neutrals for the light selection/errors.
            ThemeId::GruvboxDark => Some(Palette {
                bg: c(0x282828),
                surface: c(0x3c3836),
                fg: c(0xebdbb2),
                muted: c(0x928374),
                accent: c(0x83a598),
                selection_bg: c(0x458588),
                selection_stroke: c(0xebdbb2),
                warn: c(0xfabd2f),
                error: c(0xfb4934),
            }),
            ThemeId::GruvboxLight => Some(Palette {
                bg: c(0xfbf1c7),
                surface: c(0xebdbb2),
                fg: c(0x3c3836),
                muted: c(0x928374),
                accent: c(0x458588),
                selection_bg: c(0xd5c4a1),
                selection_stroke: c(0x3c3836),
                warn: c(0xd79921),
                error: c(0xcc241d),
            }),
            // https://ethanschoonover.com/solarized — base03/02, base0/01.
            ThemeId::SolarizedDark => Some(Palette {
                bg: c(0x002b36),
                surface: c(0x073642),
                fg: c(0x839496),
                muted: c(0x586e75),
                accent: c(0x268bd2),
                selection_bg: c(0x073642),
                selection_stroke: c(0x93a1a1),
                warn: c(0xb58900),
                error: c(0xdc322f),
            }),
            ThemeId::SolarizedLight => Some(Palette {
                bg: c(0xfdf6e3),
                surface: c(0xeee8d5),
                fg: c(0x657b83),
                muted: c(0x93a1a1),
                accent: c(0x268bd2),
                selection_bg: c(0xeee8d5),
                selection_stroke: c(0x657b83),
                warn: c(0xb58900),
                error: c(0xdc322f),
            }),
            // https://draculatheme.com — bg, current line, fg, comment.
            ThemeId::Dracula => Some(Palette {
                bg: c(0x282a36),
                surface: c(0x44475a),
                fg: c(0xf8f8f2),
                muted: c(0x6272a4),
                accent: c(0xbd93f6),
                selection_bg: c(0x44475a),
                selection_stroke: c(0xf8f8f2),
                warn: c(0xf1fa8c),
                error: c(0xff5555),
            }),
            // https://www.nordtheme.com/docs/colors-and-palettes — Polar Night
            // base/surface, Snow Storm text, Frost accent; Snow Storm bg + Polar
            // Night text for the light variant.
            ThemeId::NordDark => Some(Palette {
                bg: c(0x2e3440),
                surface: c(0x3b4252),
                fg: c(0xd8dee9),
                muted: c(0x4c566a),
                accent: c(0x88c0d0),
                selection_bg: c(0x434c5e),
                selection_stroke: c(0xd8dee9),
                warn: c(0xebcb8b),
                error: c(0xbf616a),
            }),
            ThemeId::NordLight => Some(Palette {
                bg: c(0xeceff4),
                surface: c(0xe5e9f0),
                fg: c(0x2e3440),
                muted: c(0x4c566a),
                accent: c(0x5e81ac),
                selection_bg: c(0xd8dee9),
                selection_stroke: c(0x2e3440),
                warn: c(0xebcb8b),
                error: c(0xbf616a),
            }),
            // https://github.com/catppuccin/palette (palette.json) — base /
            // surface0 / text / subtext0 + blue accent per flavor.
            ThemeId::CatppuccinMocha => Some(Palette {
                bg: c(0x1e1e2e),
                surface: c(0x313244),
                fg: c(0xcdd6f4),
                muted: c(0xa6adc8),
                accent: c(0x89b4fa),
                selection_bg: c(0x45475a),
                selection_stroke: c(0xcdd6f4),
                warn: c(0xf9e2af),
                error: c(0xf38ba8),
            }),
            ThemeId::CatppuccinMacchiato => Some(Palette {
                bg: c(0x24273a),
                surface: c(0x363a4f),
                fg: c(0xcad3f5),
                muted: c(0xa5adce),
                accent: c(0x8aadf4),
                selection_bg: c(0x494d64),
                selection_stroke: c(0xcad3f5),
                warn: c(0xeed49f),
                error: c(0xed8796),
            }),
            ThemeId::CatppuccinFrappe => Some(Palette {
                bg: c(0x303446),
                surface: c(0x414559),
                fg: c(0xc6d0f5),
                muted: c(0xa5adce),
                accent: c(0x8caaee),
                selection_bg: c(0x51576d),
                selection_stroke: c(0xc6d0f5),
                warn: c(0xe5c890),
                error: c(0xe78284),
            }),
            ThemeId::CatppuccinLatte => Some(Palette {
                bg: c(0xeff1f5),
                surface: c(0xccd0da),
                fg: c(0x4c4f69),
                muted: c(0x6c6f85),
                accent: c(0x1e66f5),
                selection_bg: c(0xbcc0cc),
                selection_stroke: c(0x4c4f69),
                warn: c(0xdf8e1d),
                error: c(0xd20f39),
            }),
            // https://github.com/sainnhe/everforest/blob/master/palette.md —
            // medium bg0/fg/grey0 plus green accent.
            ThemeId::EverforestDark => Some(Palette {
                bg: c(0x2d353b),
                surface: c(0x343f44),
                fg: c(0xd3c6aa),
                muted: c(0x7a8478),
                accent: c(0xa7c080),
                selection_bg: c(0x475258),
                selection_stroke: c(0xd3c6aa),
                warn: c(0xdbbc7f),
                error: c(0xe67e80),
            }),
            ThemeId::EverforestLight => Some(Palette {
                bg: c(0xfdf6e3),
                surface: c(0xf4f0d9),
                fg: c(0x5c6a72),
                muted: c(0x939f91),
                accent: c(0x8da101),
                selection_bg: c(0xe6e2cc),
                selection_stroke: c(0x5c6a72),
                warn: c(0xdfa000),
                error: c(0xf85552),
            }),
            // https://rosepinetheme.com/palette — base/surface, text,
            // muted + iris/pine accents; overlay steps for selections.
            ThemeId::RosePine => Some(Palette {
                bg: c(0x191724),
                surface: c(0x1f1d2e),
                fg: c(0xe0def4),
                muted: c(0x6e6a86),
                accent: c(0xc4a7e7),
                selection_bg: c(0x26233a),
                selection_stroke: c(0xe0def4),
                warn: c(0xf6c177),
                error: c(0xeb6f92),
            }),
            ThemeId::RosePineMoon => Some(Palette {
                bg: c(0x232136),
                surface: c(0x2a273f),
                fg: c(0xe0def4),
                muted: c(0x6e6a86),
                accent: c(0xc4a7e7),
                selection_bg: c(0x393552),
                selection_stroke: c(0xe0def4),
                warn: c(0xf6c177),
                error: c(0xeb6f92),
            }),
            ThemeId::RosePineDawn => Some(Palette {
                bg: c(0xfaf4ed),
                surface: c(0xfffaf3),
                fg: c(0x575279),
                muted: c(0x9893a5),
                accent: c(0x286983),
                selection_bg: c(0xf2e9e1),
                selection_stroke: c(0x575279),
                warn: c(0xea9d34),
                error: c(0xb4637a),
            }),
        }
    }

    /// Widget/rail background derived from the window surface so controls
    /// (slider rails, buttons, checkboxes) stay visible against popups and
    /// windows, which also paint `surface`. Moves ~14% toward the foreground:
    /// lighter on dark themes, darker on light themes.
    fn widget_bg(p: Palette) -> egui::Color32 {
        let t = 0.14;
        let mix = |s: u8, f: u8| {
            let d = f as f32 - s as f32;
            (s as f32 + d * t).round().clamp(0.0, 255.0) as u8
        };
        egui::Color32::from_rgb(
            mix(p.surface.r(), p.fg.r()),
            mix(p.surface.g(), p.fg.g()),
            mix(p.surface.b(), p.fg.b()),
        )
    }

    /// Full egui visuals for this theme. Scheme themes start from the matching
    /// built-in (for transfer functions, radii, alphas) and override colors.
    pub fn visuals(self) -> egui::Visuals {
        let Some(p) = self.palette() else {
            return match self {
                ThemeId::DefaultLight => egui::Visuals::light(),
                _ => egui::Visuals::dark(),
            };
        };
        let mut v = if self.is_dark() {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.panel_fill = p.bg;
        v.window_fill = p.surface;
        v.extreme_bg_color = p.surface;
        v.code_bg_color = p.surface;
        v.warn_fg_color = p.warn;
        v.error_fg_color = p.error;
        v.hyperlink_color = p.accent;
        // Slider rails paint `inactive.bg_fill` and the filled portion paints
        // `selection.bg_fill`, both sitting on `window_fill == surface`. Keep
        // all three distinct or the bars vanish on some themes.
        let rail = Self::widget_bg(p);
        let mut fill = p.selection_bg;
        if fill == p.surface || fill == rail {
            fill = p.accent;
        }
        v.selection.bg_fill = fill;
        v.selection.stroke = egui::Stroke::new(1.0, p.selection_stroke);
        v.text_cursor.stroke = egui::Stroke::new(2.0, p.accent);
        v.window_stroke = egui::Stroke::new(1.0, p.muted);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = p.bg;
        w.noninteractive.weak_bg_fill = p.bg;
        w.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.muted);
        w.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.fg);
        w.inactive.bg_fill = rail;
        w.inactive.weak_bg_fill = rail;
        w.inactive.fg_stroke = egui::Stroke::new(1.0, p.fg);
        w.hovered.bg_fill = fill;
        w.hovered.bg_stroke = egui::Stroke::new(1.0, p.accent);
        w.hovered.fg_stroke = egui::Stroke::new(1.5, p.fg);
        w.active.bg_fill = p.accent;
        w.active.bg_stroke = egui::Stroke::new(1.0, p.accent);
        w.active.fg_stroke = egui::Stroke::new(2.0, p.bg);
        w.open.bg_fill = p.surface;
        w.open.bg_stroke = egui::Stroke::new(1.0, p.muted);
        w.open.fg_stroke = egui::Stroke::new(1.0, p.fg);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for t in ThemeId::all() {
            assert_eq!(ThemeId::from_id(t.id()), Some(*t), "{}", t.id());
        }
        assert_eq!(ThemeId::all().len(), 18);
    }

    #[test]
    fn ids_unique() {
        let mut ids: Vec<&str> = ThemeId::all().iter().map(|t| t.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ThemeId::all().len());
    }

    #[test]
    fn sections_partition_all_themes() {
        let light: Vec<_> = ThemeId::all_light().collect();
        let dark: Vec<_> = ThemeId::all_dark().collect();
        assert_eq!(light.len() + dark.len(), ThemeId::all().len());
        // No overlap between sections.
        for t in &light {
            assert!(!t.is_dark(), "{}", t.id());
        }
        for t in &dark {
            assert!(t.is_dark(), "{}", t.id());
        }
        // Dracula ships dark-only upstream; Nord Light is Snow Storm.
        assert!(ThemeId::Dracula.is_dark());
        assert!(!ThemeId::NordLight.is_dark());
    }

    #[test]
    fn unknown_id_falls_back() {
        assert_eq!(ThemeId::from_id("nope"), None);
        assert_eq!(ThemeId::from_id(""), None);
    }

    #[test]
    fn every_theme_builds_with_its_background() {
        for t in ThemeId::all() {
            let v = t.visuals();
            assert_eq!(v.dark_mode, t.is_dark(), "{}", t.id());
            // Scheme themes paint panels/windows from their own palette;
            // built-ins keep egui's defaults.
            if t.palette().is_some() {
                let p = t.palette().unwrap();
                assert_eq!(v.panel_fill, p.bg, "{}", t.id());
                assert_eq!(v.window_fill, p.surface, "{}", t.id());
                let w = &v.widgets;
                assert_eq!(w.noninteractive.fg_stroke.color, p.fg, "{}", t.id());
                assert_eq!(v.hyperlink_color, p.accent, "{}", t.id());
            }
        }
    }

    #[test]
    fn slider_parts_contrast_on_windows() {
        // Slider rails (`inactive.bg_fill`) sit on `window_fill`, and the
        // filled portion (`selection.bg_fill`) sits on the rail. All three
        // must differ or the size bars disappear.
        for t in ThemeId::all() {
            let v = t.visuals();
            let rail = v.widgets.inactive.bg_fill;
            assert_ne!(rail, v.window_fill, "rail vs window {}", t.id());
            assert_ne!(v.selection.bg_fill, rail, "fill vs rail {}", t.id());
            assert_ne!(
                v.selection.bg_fill,
                v.window_fill,
                "fill vs window {}",
                t.id()
            );
        }
    }

    #[test]
    fn default_dark_matches_builtin() {
        let v = ThemeId::DefaultDark.visuals();
        assert_eq!(v.panel_fill, egui::Visuals::dark().panel_fill);
        let v = ThemeId::DefaultLight.visuals();
        assert_eq!(v.panel_fill, egui::Visuals::light().panel_fill);
    }
}
