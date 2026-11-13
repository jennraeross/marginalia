/// Bundled reading fonts (all SIL Open Font License, from Google Fonts).
///
/// - Bitter: slab serif designed for e-reading (default).
/// - Literata: serif designed for e-books.
/// - Lora: serif with calligraphic roots.
/// - Inter: neutral sans for UI-like long reading.
pub const BITTER: &[u8] = include_bytes!("../assets/fonts/Bitter.ttf");
pub const LITERATA: &[u8] = include_bytes!("../assets/fonts/Literata.ttf");
pub const LORA: &[u8] = include_bytes!("../assets/fonts/Lora.ttf");
pub const INTER: &[u8] = include_bytes!("../assets/fonts/Inter.ttf");

pub const READER_FONTS: &[(&str, &[u8])] = &[
    ("Bitter", BITTER),
    ("Literata", LITERATA),
    ("Lora", LORA),
    ("Inter", INTER),
];

pub fn default_family() -> &'static str {
    "Bitter"
}

pub fn install(ctx: &egui::Context) {
    for (name, bytes) in READER_FONTS {
        ctx.add_font(egui::FontInsert::new(
            name,
            egui::FontData::from_static(bytes),
            vec![egui::InsertFontFamily {
                family: egui::FontFamily::Name((*name).into()),
                priority: egui::FontPriority::Lowest,
            }],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_bundled() {
        for (name, bytes) in READER_FONTS {
            assert!(!bytes.is_empty(), "{name} is empty");
            // TTF/OTF magic: 0x00010000 or 'OTTO' or 'true' or 'wOF2'.
            let magic = &bytes[..4];
            assert!(
                magic == [0x00, 0x01, 0x00, 0x00]
                    || magic == *b"OTTO"
                    || magic == *b"true"
                    || magic == *b"typ1",
                "{name} does not look like a font"
            );
        }
        assert!(READER_FONTS.iter().any(|(n, _)| *n == "Bitter"));
    }

    /// The bundled files must be variable fonts whose `wght` axis actually
    /// changes advances -- otherwise the weight slider would be a no-op.
    #[test]
    fn weight_varies_advance_width() {
        let ctx = egui::Context::default();
        install(&ctx);
        // Fonts only materialize after the first frame.
        let mut full = ctx.run_ui(egui::RawInput::default(), |_| {});
        full.textures_delta.clear();
        let width = |wght: f32| -> f32 {
            ctx.fonts_mut(|fonts| {
                let mut job = egui::text::LayoutJob::single_section(
                    "Weight test body text for measurement".to_string(),
                    egui::text::TextFormat {
                        font_id: egui::FontId::new(16.0, egui::FontFamily::Name("Bitter".into())),
                        coords: egui::epaint::text::VariationCoords::new([(b"wght", wght)]),
                        ..Default::default()
                    },
                );
                job.wrap.max_width = 500.0;
                fonts.layout_job(job).size().x
            })
        };
        let light = width(300.0);
        let bold = width(700.0);
        assert!(
            bold > light,
            "wght should widen advances: {light} vs {bold}"
        );
    }
}
