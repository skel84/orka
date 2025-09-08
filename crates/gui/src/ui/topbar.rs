#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;

use crate::OrkaGuiApp;

pub(crate) fn ui_topbar(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Orka");
            ui.separator();
            ui.small_button(if app.layout.show_nav { "Hide Nav" } else { "Show Nav" })
                .clicked()
                .then(|| app.layout.show_nav = !app.layout.show_nav);
            ui.small_button(if app.layout.show_details { "Hide Details" } else { "Show Details" })
                .clicked()
                .then(|| app.layout.show_details = !app.layout.show_details);
            ui.small_button(if app.layout.show_log { "Hide Log" } else { "Show Log" })
                .clicked()
                .then(|| app.layout.show_log = !app.layout.show_log);
            ui.separator();
            // Namespace dropdown
            if let Some(i) = app.selection.selected_idx {
                if let Some(k) = app.discovery.kinds.get(i) {
                    if k.namespaced {
                        let current = if app.selection.namespace.is_empty() { "(all)".to_string() } else { app.selection.namespace.clone() };
                        egui::ComboBox::from_label("Namespace")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut app.selection.namespace, String::new(), "(all)");
                                for ns in &app.namespaces {
                                    ui.selectable_value(&mut app.selection.namespace, ns.clone(), ns);
                                }
                            });
                        ui.separator();
                    }
                }
            }
            // Kind dropdown (fallback when not using curated tree)
            egui::ComboBox::from_label("Kind")
                .selected_text(app.current_selected_kind().map(|k| crate::util::gvk_label(k)).unwrap_or_else(|| "(none)".into()))
                .show_ui(ui, |ui| {
                    for (i, k) in app.discovery.kinds.iter().enumerate() {
                        let label = crate::util::gvk_label(k);
                        let selected = app.selection.selected_idx == Some(i)
                            || app
                                .current_selected_kind()
                                .map(|kk| crate::util::gvk_label(kk) == label)
                                .unwrap_or(false);
                        if ui.selectable_label(selected, label).clicked() {
                            app.selection.selected_idx = Some(i);
                            app.selection.selected_kind = None;
                        }
                    }
                });
            ui.separator();
            // Search input and actions
            let te = egui::TextEdit::singleline(&mut app.search.query)
                .hint_text("Search: ns:prod label:app=api k:Pod payments …")
                .desired_width(360.0);
            let re = ui.add(te);
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
            }
            let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter_pressed && (re.has_focus() || !app.search.preview.is_empty()) {
                if let (Some(sel), true) = (app.search.preview_sel, !app.search.preview.is_empty()) {
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
            if ui
                .button("Go")
                .on_hover_text("Run search")
                .clicked()
            {
                app.start_search_task();
            }
            if !app.search.query.is_empty() || !app.search.hits.is_empty() {
                if ui
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
                            for (idx, (uid, score)) in app
                                .search.preview
                                .clone()
                                .into_iter()
                                .take(10)
                                .enumerate()
                            {
                                if let Some(row) = app
                                    .results.index
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
