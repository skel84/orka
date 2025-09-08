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
                // Items count with soft-cap threshold coloring
                let items = app.results.rows.len();
                let cap = app.results.soft_cap.max(1);
                let pct = (items as f32) / (cap as f32);
                let color = if pct >= app.stats.err_pct { ui.visuals().error_fg_color } else if pct >= app.stats.warn_pct { ui.visuals().warn_fg_color } else { ui.visuals().text_color() };
                ui.colored_label(color, format!("items: {}", items));
                ui.separator();
                if ui.button("Stats…").clicked() { app.stats.open = true; app.start_stats_task(); }
                if let Some(s) = &app.stats.data { ui.label(format!("shards: {}", s.shards)); }
                if let Some(epoch) = app.results.epoch { ui.separator(); ui.label(format!("epoch: {}", epoch)); }
                // Memory cap banners (static caps)
                if let Some(s) = &app.stats.data {
                    if let Some(mb) = s.max_rss_mb { ui.separator(); ui.colored_label(ui.visuals().warn_fg_color, format!("MaxRSS {} MB", mb)); }
                    if let Some(bytes) = s.max_index_bytes {
                        ui.separator();
                        // If we have current index_bytes from scraped metrics, apply threshold colors
                        if let Some(cur) = app.stats.index_bytes { 
                            let pct = if bytes > 0 { (cur as f32) / (bytes as f32) } else { 0.0 };
                            let col = if pct >= app.stats.err_pct { ui.visuals().error_fg_color } else if pct >= app.stats.warn_pct { ui.visuals().warn_fg_color } else { ui.visuals().text_color() };
                            ui.colored_label(col, format!("Index {} / {}", cur, bytes));
                        } else {
                            ui.colored_label(ui.visuals().warn_fg_color, format!("MaxIndex {} B", bytes));
                        }
                    }
                }
                if !app.search.hits.is_empty() {
                    ui.separator();
                    ui.label(format!(
                        "search hits: {}{}",
                        app.search.hits.len(),
                        if app.search.partial { " (partial)" } else { "" }
                    ));
                }
                if app.logs.dropped > 0 || (app.logs.running && app.logs.recv > 0) {
                    ui.separator();
                    // backlog usage (threshold)
                    let used = app.logs.backlog.len();
                    let cap = app.logs.backlog_cap.max(1);
                    let pct = (used as f32) / (cap as f32);
                    let col = if pct >= app.stats.err_pct { ui.visuals().error_fg_color } else if pct >= app.stats.warn_pct { ui.visuals().warn_fg_color } else { ui.visuals().text_color() };
                    ui.colored_label(col, format!("backlog: {}/{}", used, cap));
                    if app.logs.recv > 0 { ui.separator(); ui.label(format!("logs recv: {}", app.logs.recv)); }
                    if app.logs.dropped > 0 { ui.separator(); ui.colored_label(ui.visuals().error_fg_color, format!("dropped: {}", app.logs.dropped)); }
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
