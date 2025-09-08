#![forbid(unsafe_code)]

use eframe::egui;

use crate::OrkaGuiApp;

pub(crate) fn ui_statusbar(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    if !app.layout.show_log { return; }
    egui::TopBottomPanel::bottom("bottom_bar")
        .resizable(true)
        .default_height(32.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("items: {}", app.results.rows.len()));
                if !app.search.hits.is_empty() {
                    ui.separator();
                    ui.label(format!(
                        "search hits: {}{}",
                        app.search.hits.len(),
                        if app.search.partial { " (partial)" } else { "" }
                    ));
                }
                if let Some(err) = &app.last_error {
                    ui.separator();
                    ui.label(egui::RichText::new(err).color(ui.visuals().warn_fg_color));
                }
                if !app.log.is_empty() {
                    ui.separator();
                    ui.label(&app.log);
                }
            });
        });
}
