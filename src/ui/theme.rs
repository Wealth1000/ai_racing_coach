//! The look the whole window shares: one small palette, and the "?" help
//! button that rides with every action.
//!
//! The palette is the session feed's own vocabulary, promoted: green, amber
//! and red already mean faster/warning/critical there ([`crate::ui::app`]),
//! and purple — session-best on every timing screen the driver has ever
//! read — marks the one thing this tool exists to build and drive against:
//! the learned model and its personal bests. Nothing decorative; every
//! colour carries one of those meanings wherever it appears.

use eframe::egui::{self, Color32};

/// Session-best purple: the model / personal-best accent. The rarest colour
/// on a timing screen, reserved for the rarest thing here — the artefacts
/// the driver built.
pub const PURPLE: Color32 = Color32::from_rgb(0xb7, 0x8c, 0xff);
/// Faster / healthy / done. Same green the live dot uses.
pub const GREEN: Color32 = Color32::from_rgb(0x2e, 0xcc, 0x71);
/// A warning line in a job's output.
pub const AMBER: Color32 = Color32::from_rgb(0xee, 0xbb, 0x33);
/// A failed job, or a dropping stream.
pub const RED: Color32 = Color32::from_rgb(0xe7, 0x4c, 0x3c);
/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe6, 0xe1);
/// Secondary text: descriptions, hints, the "what this does" line.
pub const MUTED: Color32 = Color32::from_rgb(0x8b, 0x91, 0xa0);

/// A small group heading — "CAPTURE", "LEARN", "REVIEW" — in the muted
/// small-caps style of a timing screen's sector labels. `text` is already
/// upper-case at the call sites, so the shape lives in the style, not in a
/// transformation nobody can see.
pub fn eyebrow(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(text)
            .small()
            .weak()
            .color(MUTED)
            .strong(),
    );
    ui.add_space(4.0);
}

/// A screen's title row: a big quiet heading and, on the same line, a way
/// back to where the driver came from.
pub fn title_bar(ui: &mut egui::Ui, title: &str, back: &mut bool) {
    ui.horizontal(|ui| {
        if ui.button("‹ Back").clicked() {
            *back = true;
        }
        ui.add_space(8.0);
        ui.heading(title);
    });
    ui.add_space(6.0);
}

/// The "?" button beside an action, and the little panel it opens below
/// itself. One widget so every action explains itself the same way: a
/// click on the "?" toggles, a click anywhere else closes, and the text is
/// the full explanation the CLI carries in its `--help`.
pub fn help(ui: &mut egui::Ui, text: &str) {
    let response = ui
        .button(egui::RichText::new("?").small())
        .on_hover_cursor(egui::CursorIcon::Help);
    egui::Popup::from_toggle_button_response(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(320.0);
            ui.label(text);
        });
}

/// One action card on a sim's home screen: the button and its "?" side by
/// side, the button carrying a two-line layout job (name, then what it
/// does) so the whole action reads as one thing. Returns true when the
/// action itself — not the "?" — was clicked.
///
/// `width` is the card's width; the caller lays the grid out and hands each
/// card its share, so two columns never fight over leftovers.
pub fn action_card(
    ui: &mut egui::Ui,
    title: &str,
    desc: &str,
    help_text: &str,
    width: f32,
) -> bool {
    let help_width = 26.0;
    let button = egui::Button::new(layout_job(title, desc))
        .min_size(egui::vec2((width - help_width).max(80.0), 52.0))
        .corner_radius(6.0);
    let mut clicked = false;
    ui.horizontal(|ui| {
        clicked = ui.add(button).clicked();
        help(ui, help_text);
    });
    clicked
}

/// The two-line text inside an action card: the name in the body size, the
/// one-line description beneath it in muted small.
fn layout_job(title: &str, desc: &str) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        title,
        0.0,
        egui::TextFormat::simple(egui::FontId::proportional(15.5), TEXT),
    );
    job.append(
        "\n",
        0.0,
        egui::TextFormat::simple(egui::FontId::proportional(12.0), MUTED),
    );
    job.append(
        desc,
        0.0,
        egui::TextFormat::simple(egui::FontId::proportional(12.0), MUTED),
    );
    job.into()
}
