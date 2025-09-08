#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;

use crate::model::{Toast, ToastKind};
use crate::OrkaGuiApp;

impl OrkaGuiApp {
    pub(crate) fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        let dur = match kind { ToastKind::Error => 5000, ToastKind::Warn => 4000, _ => 3000 };
        self.toasts.push(Toast { text: text.into(), kind, created: Instant::now(), duration_ms: dur });
    }
}

pub(crate) fn draw_toasts(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    let now = Instant::now();
    app.toasts.retain(|t| now.duration_since(t.created).as_millis() < t.duration_ms as u128);
    if app.toasts.is_empty() { return; }
    egui::Area::new(egui::Id::new("toasts_area"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            // Draw newest last so it ends up at the bottom
            for t in app.toasts.iter() {
                let (bg, fg) = match t.kind {
                    ToastKind::Info => (ui.visuals().widgets.inactive.bg_fill, ui.visuals().strong_text_color()),
                    ToastKind::Success => (egui::Color32::from_rgb(34, 139, 34), egui::Color32::WHITE),
                    ToastKind::Warn => (egui::Color32::from_rgb(202, 138, 4), egui::Color32::BLACK),
                    ToastKind::Error => (egui::Color32::from_rgb(185, 28, 28), egui::Color32::WHITE),
                };
                egui::Frame::new()
                    .fill(bg)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(&t.text).color(fg));
                    });
            }
        });
}
