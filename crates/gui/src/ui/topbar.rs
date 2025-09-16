#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;

use crate::model::ToastKind;
use crate::OrkaGuiApp;

pub(crate) fn ui_topbar(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Orka");
            ui.separator();
            ui.small_button(if app.layout.show_nav {
                "Hide Nav"
            } else {
                "Show Nav"
            })
            .clicked()
            .then(|| app.layout.show_nav = !app.layout.show_nav);
            ui.small_button(if app.layout.show_log {
                "Hide Log"
            } else {
                "Show Log"
            })
            .clicked()
            .then(|| app.layout.show_log = !app.layout.show_log);
            ui.separator();
            if ui
                .small_button("Close Details Tabs")
                .on_hover_text("Close all Details tabs")
                .clicked()
            {
                app.close_all_details_tabs();
            }
            if app.atlas_enabled
                && ui
                    .small_button("Open Atlas")
                    .on_hover_text("Open the global Atlas view")
                    .clicked()
            {
                app.open_atlas_tab();
            }
            if !app.detached.is_empty() {
                let label = format!("Reattach All ({})", app.detached.len());
                if ui
                    .small_button(label)
                    .on_hover_text("Close detached windows and reopen as tabs")
                    .clicked()
                {
                    let ids: Vec<(egui::ViewportId, orka_core::Uid)> = app
                        .detached
                        .iter()
                        .map(|w| (w.meta.id, w.meta.uid))
                        .collect();
                    for (id, uid) in &ids {
                        app.open_details_tab_for(*uid);
                        ui.ctx()
                            .send_viewport_cmd_to(*id, egui::ViewportCommand::Close);
                    }
                    app.detached.clear();
                    app.toast(format!("reattached {}", ids.len()), ToastKind::Info);
                }
            }
            ui.separator();
            // Kubernetes context selector (replaces Kind/Namespace selectors)
            let current_ctx = app
                .current_context
                .clone()
                .unwrap_or_else(|| "(default)".to_string());
            egui::ComboBox::from_label("Context")
                .selected_text(current_ctx)
                .show_ui(ui, |ui| {
                    let contexts = app.contexts.clone();
                    for ctx_name in contexts {
                        let selected = app.current_context.as_deref() == Some(ctx_name.as_str());
                        if ui.selectable_label(selected, &ctx_name).clicked() {
                            app.on_context_selected(ctx_name);
                        }
                    }
                });
            ui.separator();
            // Search input and actions
            let te = egui::TextEdit::singleline(&mut app.search.query)
                .hint_text("Search: ns:prod label:app=api k:Pod payments …")
                .desired_width(360.0);
            let re = ui.add(te);
            if app.search.need_focus {
                re.request_focus();
                app.search.need_focus = false;
            }
            if re.changed() {
                app.search.changed_at = Some(Instant::now());
            }
            if re.has_focus() {
                let len = app.search.preview.len();
                if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                    let next = app.search.preview_sel.map(|i| i + 1).unwrap_or(0);
                    let idx = if len == 0 { 0 } else { next % len };
                    app.search.preview_sel = Some(idx);
                }
                if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                    let cur = app.search.preview_sel.unwrap_or(0);
                    let prev = if cur == 0 { len - 1 } else { cur - 1 };
                    app.search.preview_sel = Some(prev);
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    app.search.query.clear();
                    app.search.hits.clear();
                    app.search.explain = None;
                    app.search.partial = false;
                    app.search.preview.clear();
                    app.search.preview_sel = None;
                }
            }
            let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter_pressed && (re.has_focus() || !app.search.preview.is_empty()) {
                if let (Some(sel), true) = (app.search.preview_sel, !app.search.preview.is_empty())
                {
                    if let Some((uid, _)) = app.search.preview.get(sel).copied() {
                        if let Some(i) = app.results.index.get(&uid).copied() {
                            if let Some(row) = app.results.rows.get(i).cloned() {
                                app.select_row(row);
                            }
                        }
                    }
                } else {
                    app.start_search_task();
                }
            }
            if ui.button("Go").on_hover_text("Run search").clicked() {
                app.start_search_task();
            }
            if (!app.search.query.is_empty() || !app.search.hits.is_empty())
                && ui
                    .button("×")
                    .on_hover_text("Clear search overlay")
                    .clicked()
            {
                app.search.query.clear();
                app.search.hits.clear();
                app.search.explain = None;
                app.search.partial = false;
                app.search.preview.clear();
                app.search.preview_sel = None;
            }
            if app.search.task.is_some() {
                ui.add(egui::Spinner::new());
            }

            // Debounced live preview
            if let Some(t0) = app.search.changed_at {
                if t0.elapsed().as_millis() as u64 >= app.search.debounce_ms {
                    app.rebuild_search_preview();
                    app.search.changed_at = None;
                }
            }

            // Popup preview under the search box
            if !app.search.query.trim().is_empty() && !app.search.preview.is_empty() {
                let pos = re.rect.left_bottom() + egui::vec2(0.0, 4.0);
                egui::Area::new("search_preview".into())
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .show(ui.ctx(), |ui| {
                        let frame = egui::Frame::new()
                            .fill(ui.visuals().extreme_bg_color)
                            .stroke(egui::Stroke::new(
                                1.0,
                                ui.visuals().widgets.noninteractive.bg_stroke.color,
                            ))
                            .outer_margin(egui::Margin::same(4))
                            .inner_margin(egui::Margin::symmetric(8, 6));
                        frame.show(ui, |ui| {
                            ui.set_width(420.0);
                            ui.label(egui::RichText::new("Live preview").strong());
                            ui.separator();
                            for (idx, (uid, score)) in
                                app.search.preview.clone().into_iter().take(10).enumerate()
                            {
                                if let Some(row) = app
                                    .results
                                    .index
                                    .get(&uid)
                                    .and_then(|i| app.results.rows.get(*i))
                                {
                                    let ns = row.namespace.as_deref().unwrap_or("-");
                                    let name = &row.name;
                                    let text = format!("{}/{}   ({:.2})", ns, name, score);
                                    let is_sel = app.search.preview_sel == Some(idx);
                                    let clicked = ui
                                        .selectable_label(
                                            is_sel,
                                            egui::RichText::new(text).monospace(),
                                        )
                                        .clicked();
                                    if clicked {
                                        app.select_row(row.clone());
                                    }
                                }
                            }
                            if ui.small_button("Open full results ↵").clicked() {
                                app.start_search_task();
                            }
                        });
                    });
            }
        });
    });
}
