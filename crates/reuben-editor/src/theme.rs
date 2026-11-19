//! The editor's look, adapted from impracticalinstruments.com: ink on paper, the site's blue wash
//! behind the canvas, yellow for what is selected, rust for what the pointer is over. Every colour
//! the editor draws is decided here; panels read them back through `ui.visuals()`.
//!
//! Type is two faces. Cuomotype (CC0) sets headings and section titles; its weathered glyphs
//! smear below about 14 px, so body text, numbers and the canvas are Courier Prime (OFL). Each
//! face's licence sits beside it in `assets/fonts/`.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke,
    Style, TextStyle, Theme, Visuals,
};

use crate::canvas::PortKind;

const DISPLAY: &str = "cuomotype";
const BODY: &str = "courier-prime";

const INK: Color32 = Color32::from_rgb(0x1E, 0x1C, 0x19);
const STRONG: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
const WEAK: Color32 = Color32::from_rgb(0x5C, 0x58, 0x50);
const PAPER: Color32 = Color32::from_rgb(0xF7, 0xF5, 0xEF);
/// A sheet laid on the paper: node boxes, menus, text fields.
const CARD: Color32 = Color32::from_rgb(0xFD, 0xFC, 0xF9);
/// The site's wash, `rgba(117, 155, 174, 0.16)`, flattened onto [`PAPER`].
const WASH: Color32 = Color32::from_rgb(0xE3, 0xE7, 0xE5);
const STRIPE: Color32 = Color32::from_rgb(0xF0, 0xED, 0xE4);
const BUTTON: Color32 = Color32::from_rgb(0xEE, 0xEB, 0xE2);
const HOVER: Color32 = Color32::from_rgb(0xE9, 0xE4, 0xD6);
const PRESSED: Color32 = Color32::from_rgb(0xDD, 0xD6, 0xC3);
/// The site's band blue, darkened until a 1 px rule holds 3:1 against [`WASH`].
const RULE: Color32 = Color32::from_rgb(0x5E, 0x7F, 0x8F);
const YELLOW: Color32 = Color32::from_rgb(0xFF, 0xF2, 0x00);
const RUST: Color32 = Color32::from_rgb(0xBC, 0x4C, 0x26);
/// The site's blue, darkened from `#2076a3` until it reads as text at 4.5:1 on every ground.
const BLUE: Color32 = Color32::from_rgb(0x18, 0x5D, 0x85);
const WARN: Color32 = Color32::from_rgb(0x7A, 0x50, 0x00);
const ERROR: Color32 = Color32::from_rgb(0xB0, 0x10, 0x2E);

/// The controls panel's keyboard.
pub const KEY_WHITE: Color32 = CARD;
pub const KEY_BLACK: Color32 = INK;
pub const KEY_EDGE: Color32 = WEAK;

/// Make the editor look like this module says, on every frame from the next one.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(Theme::Light);
    ctx.set_style_of(Theme::Light, style());
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        DISPLAY.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Cuomotype.otf"
        ))),
    );
    fonts.font_data.insert(
        BODY.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/CourierPrime-Regular.ttf"
        ))),
    );
    // egui's own faces stay behind each as fallbacks, for the arrows and symbols the two
    // typewriter faces do not draw.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, BODY.into());
    }
    let mut display = vec![DISPLAY.to_owned()];
    display.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(DISPLAY.into()), display);
    fonts
}

fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DISPLAY.into()))
}

fn style() -> Style {
    let mut style = Style {
        visuals: visuals(),
        ..Style::default()
    };
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(10.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Heading, display(22.0)),
    ]
    .into();
    style
}

fn visuals() -> Visuals {
    let mut v = Visuals::light();
    v.override_text_color = None;
    v.weak_text_color = Some(WEAK);
    v.hyperlink_color = BLUE;
    v.faint_bg_color = STRIPE;
    // The canvas's ground; text fields keep a sheet of their own.
    v.extreme_bg_color = WASH;
    v.text_edit_bg_color = Some(CARD);
    v.code_bg_color = STRIPE;
    v.warn_fg_color = WARN;
    v.error_fg_color = ERROR;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, RULE);
    v.panel_fill = PAPER;
    v.selection.bg_fill = YELLOW;
    // The colour a selected label's text takes, and the outline of a selected node or held key.
    // Ink rather than a hue, so it never reads as a port kind.
    v.selection.stroke = Stroke::new(2.0, STRONG);
    v.text_cursor.stroke = Stroke::new(2.0, BLUE);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PAPER;
    w.noninteractive.weak_bg_fill = PAPER;
    w.noninteractive.bg_stroke = Stroke::new(1.0, RULE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, INK);
    w.inactive.bg_fill = BUTTON;
    w.inactive.weak_bg_fill = BUTTON;
    w.inactive.bg_stroke = Stroke::new(1.0, RULE);
    w.inactive.fg_stroke = Stroke::new(1.0, INK);
    w.hovered.bg_fill = HOVER;
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, RUST);
    w.hovered.fg_stroke = Stroke::new(1.5, STRONG);
    w.active.bg_fill = PRESSED;
    w.active.weak_bg_fill = PRESSED;
    w.active.bg_stroke = Stroke::new(1.0, RUST);
    w.active.fg_stroke = Stroke::new(2.0, STRONG);
    w.open.bg_fill = HOVER;
    w.open.weak_bg_fill = HOVER;
    w.open.bg_stroke = Stroke::new(1.0, RULE);
    w.open.fg_stroke = Stroke::new(1.0, STRONG);
    for widget in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        widget.corner_radius = CornerRadius::same(1);
    }
    v
}

/// How much of its colour a wire keeps while nothing is selected.
pub const RESTING_WIRE: f32 = 0.8;

/// A port's colour, and the colour of the wires it sends or takes.
pub fn kind_color(kind: PortKind) -> Color32 {
    match kind {
        PortKind::Signal => RUST,
        PortKind::Value => Color32::from_rgb(0x1A, 0x6A, 0x94),
        PortKind::Int => Color32::from_rgb(0x0F, 0x70, 0x69),
        PortKind::Enum => Color32::from_rgb(0x6E, 0x4B, 0xA8),
        PortKind::Message => Color32::from_rgb(0x35, 0x6E, 0x23),
        PortKind::Harmony => Color32::from_rgb(0xB0, 0x30, 0x7A),
        PortKind::Vocab => Color32::from_rgb(0x6E, 0x6A, 0x00),
        PortKind::Text => Color32::from_rgb(0x7A, 0x4E, 0x2D),
        PortKind::Arg => Color32::from_rgb(0x2B, 0x2B, 0x2B),
        PortKind::Unknown => Color32::from_rgb(0x6B, 0x6B, 0x6B),
    }
}

/// A panel's section title, set in capitals in the display face, as the site sets its labels.
/// On a context [`install`] has not set up, egui would panic on the unbound display family, so the
/// title falls back to the body face.
pub fn section(ui: &mut egui::Ui, title: &str) -> egui::Response {
    let font = display(16.0);
    let bound = ui.fonts(|f| f.families().contains(&font.family));
    let text = RichText::new(title.to_uppercase()).color(ui.visuals().strong_text_color());
    ui.label(if bound {
        text.font(font)
    } else {
        text.strong()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [PortKind; 10] = [
        PortKind::Signal,
        PortKind::Value,
        PortKind::Int,
        PortKind::Enum,
        PortKind::Message,
        PortKind::Harmony,
        PortKind::Vocab,
        PortKind::Text,
        PortKind::Arg,
        PortKind::Unknown,
    ];

    fn linear(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG 2.x relative luminance.
    fn luminance(c: Color32) -> f64 {
        0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// CIELAB under D65, for a perceptual distance between two colours.
    fn lab(c: Color32) -> [f64; 3] {
        let (r, g, b) = (linear(c.r()), linear(c.g()), linear(c.b()));
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
        let f = |t: f64| {
            if t > 0.008856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        [
            116.0 * f(y) - 16.0,
            500.0 * (f(x) - f(y)),
            200.0 * (f(y) - f(z)),
        ]
    }

    fn delta_e(a: Color32, b: Color32) -> f64 {
        let (a, b) = (lab(a), lab(b));
        a.iter()
            .zip(b)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f64>()
            .sqrt()
    }

    type Named = (&'static str, Color32);

    /// Every colour enabled text is drawn in, against every ground text is drawn on. Not every
    /// pair occurs on screen; checking them all means no new placement can fall below the line.
    /// Disabled widgets fade their text to `disabled_alpha`, which WCAG exempts.
    fn text_and_grounds(v: &Visuals) -> (Vec<Named>, Vec<Named>) {
        let w = &v.widgets;
        let text = vec![
            ("text", v.text_color()),
            ("strong", v.strong_text_color()),
            ("weak", v.weak_text_color()),
            ("warn", v.warn_fg_color),
            ("error", v.error_fg_color),
            ("hyperlink", v.hyperlink_color),
            ("selected", v.selection.stroke.color),
            ("button", w.inactive.fg_stroke.color),
            ("hovered", w.hovered.fg_stroke.color),
            ("pressed", w.active.fg_stroke.color),
            ("open", w.open.fg_stroke.color),
        ];
        let grounds = vec![
            ("panel", v.panel_fill),
            ("window / node", v.window_fill),
            ("canvas", v.extreme_bg_color),
            ("text field", v.text_edit_bg_color()),
            ("stripe", v.faint_bg_color),
            ("code", v.code_bg_color),
            ("selection", v.selection.bg_fill),
            ("label", w.noninteractive.bg_fill),
            ("button / pipe", w.inactive.bg_fill),
            ("button weak", w.inactive.weak_bg_fill),
            ("hovered", w.hovered.bg_fill),
            ("pressed", w.active.bg_fill),
            ("open", w.open.bg_fill),
        ];
        (text, grounds)
    }

    #[test]
    fn all_text_meets_wcag_aa_against_every_ground() {
        let v = visuals();
        let (text, grounds) = text_and_grounds(&v);
        let mut failures = Vec::new();
        for (fg_name, fg) in &text {
            for (bg_name, bg) in &grounds {
                let ratio = contrast(*fg, *bg);
                if ratio < 4.5 {
                    failures.push(format!("{fg_name} on {bg_name}: {ratio:.2}"));
                }
            }
        }
        assert!(failures.is_empty(), "below 4.5:1: {failures:#?}");
    }

    #[test]
    fn the_keyboard_keys_meet_wcag_non_text_contrast() {
        let held = visuals().selection;
        assert!(contrast(KEY_BLACK, KEY_WHITE) >= 3.0);
        assert!(contrast(KEY_EDGE, KEY_WHITE) >= 3.0);
        assert!(contrast(held.bg_fill, KEY_BLACK) >= 3.0);
        // A held white key is told from its neighbours by its outline.
        assert!(contrast(held.stroke.color, KEY_WHITE) >= 3.0);
    }

    /// `c` laid over `ground` at `alpha`, as egui paints a `gamma_multiply`'d colour.
    fn over(c: Color32, alpha: f32, ground: Color32) -> Color32 {
        let mix =
            |a: u8, b: u8| (f32::from(a) * alpha + f32::from(b) * (1.0 - alpha)).round() as u8;
        Color32::from_rgb(
            mix(c.r(), ground.r()),
            mix(c.g(), ground.g()),
            mix(c.b(), ground.b()),
        )
    }

    /// WCAG's 3:1 for graphical objects: a port against the canvas and the node box it sits on,
    /// and a wire at rest against the canvas.
    #[test]
    fn every_port_kind_stands_out_from_the_canvas_and_its_box() {
        let v = visuals();
        let canvas = v.extreme_bg_color;
        for kind in KINDS {
            let c = kind_color(kind);
            for (name, fg, ground) in [
                ("port on canvas", c, canvas),
                ("port on node", c, v.window_fill),
                ("resting wire", over(c, RESTING_WIRE, canvas), canvas),
            ] {
                let ratio = contrast(fg, ground);
                assert!(ratio >= 3.0, "{kind:?} {name}: {ratio:.2}");
            }
        }
    }

    /// 2.3 in CIELAB is a just-noticeable difference; 25 between full colours and 20 between
    /// resting wires is a different colour at a glance on a thin line. Wires dimmed behind a
    /// selection are meant to recede, and are not held to this.
    #[test]
    fn port_kinds_are_distinguishable_from_one_another() {
        let canvas = visuals().extreme_bg_color;
        for (i, a) in KINDS.iter().enumerate() {
            for b in &KINDS[i + 1..] {
                let (ca, cb) = (kind_color(*a), kind_color(*b));
                let d = delta_e(ca, cb);
                assert!(d >= 25.0, "{a:?} and {b:?} are {d:.1} apart");
                let d = delta_e(
                    over(ca, RESTING_WIRE, canvas),
                    over(cb, RESTING_WIRE, canvas),
                );
                assert!(d >= 20.0, "resting {a:?} and {b:?} wires are {d:.1} apart");
            }
        }
    }

    /// A node's outline says selected, error or warning; none of them may pass for a port kind.
    #[test]
    fn status_outlines_are_not_mistaken_for_port_kinds() {
        let v = visuals();
        for (name, status) in [
            ("selected", v.selection.stroke.color),
            ("error", v.error_fg_color),
            ("warning", v.warn_fg_color),
        ] {
            for kind in KINDS {
                let d = delta_e(status, kind_color(kind));
                assert!(d >= 15.0, "{name} and {kind:?} are {d:.1} apart");
            }
        }
    }

    #[test]
    fn body_text_is_courier_prime_and_headings_cuomotype() {
        let ctx = egui::Context::default();
        install(&ctx);
        // Fonts take effect at the start of a frame; nothing here paints.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        let width = |font: FontId, c: char| ctx.fonts_mut(|f| f.glyph_width(&font, c));
        // Courier Prime is monospaced at 0.6 em; egui's proportional 'i' is far narrower.
        assert!((width(FontId::proportional(100.0), 'i') - 60.0).abs() < 0.5);
        // Cuomotype is proportional, with an 'i' of 0.34 em.
        assert!((width(display(100.0), 'i') - 34.0).abs() < 0.5);
        // The canvas sizes its boxes on the monospace advance; Courier Prime's is 0.6 em.
        assert!((width(FontId::monospace(100.0), 'W') - 60.0).abs() < 0.5);
    }

    #[test]
    fn a_section_title_draws_without_the_theme_installed() {
        egui::Context::default()
            .run_ui(egui::RawInput::default(), |ui| {
                section(ui, "Files");
            })
            .drop_without_applying_deltas();
    }
}
