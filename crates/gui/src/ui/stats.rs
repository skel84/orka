#![forbid(unsafe_code)]

use eframe::egui;

use crate::OrkaGuiApp;

pub(crate) fn ui_stats_modal(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    if !app.stats.open {
        return;
    }
    let mut open = app.stats.open;
    egui::Window::new("Orka Stats")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -20.0))
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.heading("Runtime");
                ui.separator();

                if app.stats.loading && app.stats.data.is_none() {
                    ui.label("Loading…");
                }
                if let Some(err) = &app.stats.last_error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                if let Some(s) = &app.stats.data {
                    grid_kv(ui, "Pipelines", &s.shards.to_string());
                    grid_kv(ui, "Relist (secs)", &s.relist_secs.to_string());
                    grid_kv(
                        ui,
                        "Watch backoff max (secs)",
                        &s.watch_backoff_max_secs.to_string(),
                    );
                    if let Some(v) = s.max_labels_per_obj {
                        grid_kv(ui, "Max labels per object", &v.to_string());
                    }
                    if let Some(v) = s.max_annos_per_obj {
                        grid_kv(ui, "Max annotations per object", &v.to_string());
                    }
                    if let Some(v) = s.max_postings_per_key {
                        grid_kv(ui, "Max postings per key", &v.to_string());
                    }
                    if let Some(v) = s.max_rss_mb {
                        grid_threshold(
                            ui,
                            "Max RSS (MB)",
                            v as u64,
                            None,
                            app.stats.warn_pct,
                            app.stats.err_pct,
                        );
                    }
                    if let Some(v) = s.max_index_bytes {
                        grid_threshold(
                            ui,
                            "Index bytes",
                            v as u64,
                            app.stats.index_bytes,
                            app.stats.warn_pct,
                            app.stats.err_pct,
                        );
                    }
                    if let Some(addr) = &s.metrics_addr {
                        let url = format!("http://{}/metrics", addr);
                        ui.horizontal(|ui| {
                            ui.label("Metrics:");
                            ui.hyperlink(url);
                        });
                    } else {
                        grid_kv(ui, "Metrics", "(not set)");
                    }
                    ui.separator();
                }

                ui.heading("Traffic (since start)");
                ui.separator();
                if let Some(s) = &app.stats.data {
                    if let (Some(a), Some(b), Some(c)) = (
                        s.traffic_snapshot_bytes,
                        s.traffic_watch_bytes,
                        s.traffic_details_bytes,
                    ) {
                        grid_kv(ui, "Snapshot bytes", &fmt_bytes(a));
                        grid_kv(ui, "Watch bytes", &fmt_bytes(b));
                        grid_kv(ui, "Details bytes", &fmt_bytes(c));
                    } else {
                        grid_kv(ui, "Snapshot bytes", "(n/a)");
                        grid_kv(ui, "Watch bytes", "(n/a)");
                        grid_kv(ui, "Details bytes", "(n/a)");
                    }
                    ui.separator();
                }

                ui.heading("UI Pressure");
                ui.separator();
                // Minimal local counters
                let dropped = app.logs.dropped;
                let recv = app.logs.recv;
                grid_kv(ui, "Logs received", &recv.to_string());
                let dropped_label = format!("{}", dropped);
                let dropped_color = if dropped > 0 {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                grid_kv_colored(ui, "Logs dropped", &dropped_label, dropped_color);
                // Results cap threshold
                let soft_cap = app.results.soft_cap as f32;
                let rows = app.results.rows.len() as f32;
                let pct = if soft_cap > 0.0 { rows / soft_cap } else { 0.0 };
                let color = if pct >= app.stats.err_pct {
                    ui.visuals().error_fg_color
                } else if pct >= app.stats.warn_pct {
                    ui.visuals().warn_fg_color
                } else {
                    ui.visuals().text_color()
                };
                grid_kv_colored(
                    ui,
                    "Rows (current)",
                    &format!("{} / {}", app.results.rows.len(), app.results.soft_cap),
                    color,
                );

                ui.separator();
                ui.heading("Explain (Search)");
                ui.separator();
                if let Some(ex) = &app.search.explain {
                    grid_kv(ui, "Total hits", &ex.total.to_string());
                    grid_kv(ui, "After namespace", &ex.after_ns.to_string());
                    grid_kv(ui, "After label keys", &ex.after_label_keys.to_string());
                    grid_kv(ui, "After labels", &ex.after_labels.to_string());
                    grid_kv(ui, "After anno keys", &ex.after_anno_keys.to_string());
                    grid_kv(ui, "After annotations", &ex.after_annos.to_string());
                    grid_kv(ui, "After fields", &ex.after_fields.to_string());
                    if app.search.partial {
                        ui.label(
                            egui::RichText::new(
                                "partial results — recovering from backlog/overflow",
                            )
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                } else {
                    ui.label("Run a search to populate explain statistics.");
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Refresh").clicked() {
                        app.start_stats_task();
                    }
                    if ui.button("Close").clicked() {
                        app.stats.open = false;
                    }
                });
            });
        });
    app.stats.open = open;
}

fn grid_kv(ui: &mut egui::Ui, k: &str, v: &str) {
    egui::Grid::new(format!("stats_{}", k))
        .num_columns(2)
        .spacing([10.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(k).strong());
            ui.label(v);
            ui.end_row();
        });
}

fn grid_kv_colored(ui: &mut egui::Ui, k: &str, v: &str, color: egui::Color32) {
    egui::Grid::new(format!("stats_{}", k))
        .num_columns(2)
        .spacing([10.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(k).strong());
            ui.colored_label(color, v);
            ui.end_row();
        });
}

fn grid_threshold(ui: &mut egui::Ui, k: &str, max: u64, cur_opt: Option<u64>, warn: f32, err: f32) {
    match cur_opt {
        Some(cur) => {
            let pct = if max > 0 {
                (cur as f32) / (max as f32)
            } else {
                0.0
            };
            let color = if pct >= err {
                ui.visuals().error_fg_color
            } else if pct >= warn {
                ui.visuals().warn_fg_color
            } else {
                ui.visuals().text_color()
            };
            let s = format!("{} / {}  ({:.1}%)", cur, max, pct * 100.0);
            grid_kv_colored(ui, k, &s, color);
        }
        None => grid_kv(ui, k, &format!("max {} (current n/a)", max)),
    }
}

fn fmt_bytes(v: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let f = v as f64;
    if f >= GB {
        format!("{:.2} GiB", f / GB)
    } else if f >= MB {
        format!("{:.2} MiB", f / MB)
    } else if f >= KB {
        format!("{:.2} KiB", f / KB)
    } else {
        format!("{} B", v)
    }
}
