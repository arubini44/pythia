//! Small icon helpers on top of `orui_ui::icons`.

use egui::{Response, Ui};
use orui_ui::icons::{Icon, IconSize};

/// A compact, frameless icon button (14 pt) for dense headers and lists.
pub fn small_icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let image = if icon.is_colored() {
        icon.sized(ui, IconSize::Small)
    } else {
        icon.mono(egui::Color32::WHITE)
            .fit_to_exact_size(egui::vec2(
                IconSize::Small.points(),
                IconSize::Small.points(),
            ))
    };
    ui.add(
        egui::Button::image(image)
            .frame_when_inactive(false)
            .image_tint_follows_text_color(!icon.is_colored()),
    )
    .on_hover_text(tooltip)
}
