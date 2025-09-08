#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;
use tracing::info;

use orka_api::ResourceRef;
use orka_core::LiteObj;

use crate::util::gvk_label;
use super::{OrkaGuiApp, UiUpdate};

impl OrkaGuiApp {
    fn ui_edit(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Edit")
            .default_open(false)
            .show(ui, |ui| {
                // Toolbar
                ui.horizontal(|ui| {
                    if ui.button("Reset to live").on_hover_text("Reset editor to current Details").clicked() {
                        self.edit.buffer = self.edit.original.clone();
                        self.edit.dirty = false;
                        self.edit.status.clear();
                    }
                    if ui.button("Dry-run").on_hover_text("Server-side dry-run; show diff summary").clicked() {
                        self.start_edit_dry_run_task();
                    }
                    if ui.button("Diff").on_hover_text("Compute diff vs live and last-applied").clicked() {
                        self.start_edit_diff_task();
                    }
                    if ui.button("Apply").on_hover_text("Server-side apply (SSA)").clicked() {
                        self.start_edit_apply_task();
                    }
                    if self.edit.running { ui.add(egui::Spinner::new()); }
                    if !self.edit.status.is_empty() {
                        ui.separator();
                        ui.label(&self.edit.status);
                    }
                });
                ui.add_space(4.0);
                // Editor
                // Editor with line numbers and indent guides.
                let lines_count = self.edit.buffer.lines().count().max(1);
                let digits = ((lines_count as f32).log10().floor() as usize + 1).max(2);
                let mono = egui::TextStyle::Monospace;
                let row_h = ui.text_style_height(&mono);
                let gutter_w = 8.0 + digits as f32 * 8.0;
                egui::ScrollArea::horizontal().id_salt("edit_scroll_h").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Left gutter with line numbers
                        let mut nums = String::with_capacity(digits * lines_count + lines_count);
                        for i in 1..=lines_count { let _ = std::fmt::write(&mut nums, format_args!("{i:>width$}\n", width = digits)); }
                        let total_h = (lines_count as f32) * row_h + 8.0;
                        let (rect, _resp) = ui.allocate_exact_size(egui::vec2(gutter_w, total_h), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
                        ui.painter().text(
                            rect.left_top() + egui::vec2(4.0, 4.0),
                            egui::Align2::LEFT_TOP,
                            nums,
                            mono.resolve(ui.style()),
                            ui.visuals().weak_text_color(),
                        );

                        // Right text editor
                        let mut layouter = crate::util::highlight::yaml_layouter();
                        let id = ui.make_persistent_id("edit_text");
                        let out = egui::TextEdit::multiline(&mut self.edit.buffer)
                            .id(id)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(lines_count)
                            .desired_width(f32::INFINITY)
                            .frame(true)
                            .layouter(&mut layouter)
                            .show(ui);
                        if out.response.changed() { self.edit.dirty = self.edit.buffer != self.edit.original; }

                        // Current line highlight (based on caret position)
                        if let Some(cr) = out.cursor_range {
                            let idx = cr.primary.index.min(self.edit.buffer.len());
                            let line_idx = self.edit.buffer[..idx].chars().filter(|&c| c == '\n').count();
                            let y = rect.top() + 4.0 + (line_idx as f32) * row_h;
                            let bg = ui.visuals().widgets.inactive.bg_fill.linear_multiply(0.35);
                            let hl_rect = egui::Rect::from_min_size(egui::pos2(rect.left(), y), egui::vec2(rect.width(), row_h));
                            ui.painter().rect_filled(hl_rect, 0.0, bg);
                        }

                        // Indent guides overlay
                        let rect = out.response.rect;
                        let space_w = ui.fonts(|f| f.glyph_width(&mono.resolve(ui.style()), ' '));
                        let step = (2.0 * space_w).max(2.0);
                        let mut y = rect.top() + 4.0;
                        for line in self.edit.buffer.split_inclusive('\n') {
                            let spaces = line.chars().take_while(|&c| c == ' ').count();
                            let levels = (spaces as f32 / 2.0).floor() as usize;
                            for lvl in 1..=levels {
                                let x = rect.left() + 4.0 + (lvl as f32) * step;
                                ui.painter().line_segment(
                                    [egui::pos2(x, y), egui::pos2(x, y + row_h - 2.0)],
                                    egui::Stroke::new(1.0, ui.visuals().faint_bg_color),
                                );
                            }
                            y += row_h;
                        }
                    });
                });
            });
    }

    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        if !self.selected_is_pod() { return; }
        egui::CollapsingHeader::new("Logs (Pod)")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.logs.follow, "Follow");
                    ui.label("Tail:");
                    let mut tail = self.logs.tail_lines.unwrap_or(0);
                    if ui.add(egui::DragValue::new(&mut tail).range(0..=10000)).on_hover_text("Tail last N lines; 0 disables").changed() {
                        self.logs.tail_lines = if tail <= 0 { None } else { Some(tail as i64) };
                    }
                    ui.separator();
                    ui.label("Container:");
                    if self.logs.containers.is_empty() {
                        ui.label(egui::RichText::new("(none)").weak());
                    } else {
                        let current = self.logs.container.clone().unwrap_or_else(|| self.logs.containers.get(0).cloned().unwrap_or_default());
                        let mut selected = current.clone();
                        egui::ComboBox::from_id_salt("logs_container_select")
                            .selected_text(selected.clone())
                            .show_ui(ui, |ui| {
                                for name in &self.logs.containers {
                                    ui.selectable_value(&mut selected, name.clone(), name);
                                }
                            });
                        if selected != current { self.logs.container = Some(selected); }
                    }
                    ui.separator();
                    ui.label("Grep:");
                    ui.add(egui::TextEdit::singleline(&mut self.logs.grep).desired_width(160.0));
                    if ui.button("Clear").on_hover_text("Clear backlog").clicked() { self.logs.backlog.clear(); }
                    if !self.logs.running {
                        if ui.button("Start").on_hover_text("Start streaming logs").clicked() { self.start_logs_task(); }
                    } else {
                        if ui.button("Stop").on_hover_text("Stop logs").clicked() { self.stop_logs_task(); }
                    }
                });
                ui.add_space(4.0);
                // Render log lines
                let re: Option<regex::Regex> = if self.logs.grep.trim().is_empty() { None } else { regex::Regex::new(&self.logs.grep).ok() };
                let mut buf = String::new();
                let mut shown = 0usize;
                let max_lines: usize = 1000; // cap per paint for perf
                for line in self.logs.backlog.iter().rev() {
                    if let Some(r) = &re { if !r.is_match(line) { continue; } }
                    buf.push_str(line);
                    if !line.ends_with('\n') { buf.push('\n'); }
                    shown += 1;
                    if shown >= max_lines { break; }
                }
                let display = if shown == 0 { String::new() } else { buf.lines().rev().collect::<Vec<_>>().join("\n") };
                let mut binding = display;
                let te = egui::TextEdit::multiline(&mut binding)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(20)
                    .desired_width(f32::INFINITY)
                    .interactive(false);
                ui.add(te);
            });
    }
    pub(crate) fn ui_explain(&mut self, ui: &mut egui::Ui) {
        let has = self.search.explain.is_some();
        egui::CollapsingHeader::new("Explain (Search)")
            .default_open(false)
            .show(ui, |ui| {
                if let Some(ex) = &self.search.explain {
                    ui.label(format!(
                        "total={} ns={} label_keys={} labels={} anno_keys={} annos={} fields={}",
                        ex.total, ex.after_ns, ex.after_label_keys, ex.after_labels, ex.after_anno_keys, ex.after_annos, ex.after_fields
                    ));
                    if self.search.partial {
                        ui.label(egui::RichText::new("partial results — recovering from backlog/overflow").color(ui.visuals().warn_fg_color));
                    }
                } else {
                    ui.label("Run a search to populate explain statistics.");
                }
            });
        if has { ui.separator(); }
    }
    pub(crate) fn ui_details(&mut self, ui: &mut egui::Ui) {
        ui.heading("Details");
        egui::ScrollArea::vertical()
            .id_salt("details_scroll")
            .show(ui, |ui| {
                // Contextual actions bar (logs, exec, pf, scale, etc.)
                crate::ui::actions::ui_actions_bar(self, ui);
                ui.separator();
                // Explain section (collapsed by default)
                self.ui_explain(ui);
                // Edit section
                self.ui_edit(ui);
                // Logs section (Pods only)
                self.ui_logs(ui);
                if self.details.buffer.is_empty() {
                    ui.label("Select a row to view details");
                } else {
                    // Read-only YAML with syntect highlighting
                    let mut layouter = crate::util::highlight::yaml_layouter();
                    let te = egui::TextEdit::multiline(&mut self.details.buffer)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(24)
                        .desired_width(f32::INFINITY)
                        .interactive(false)
                        .layouter(&mut layouter);
                    ui.add(te);
                }
            });
    }

    pub(crate) fn select_row(&mut self, it: LiteObj) {
        info!(uid = ?it.uid, name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "details: selecting row");
        self.details.selected = Some(it.uid);
        self.details.buffer.clear();
        // Clear pod-specific logs metadata on selection change
        self.logs.containers.clear();
        self.logs.container = None;
        // cancel previous detail task if any
        if let Some(stop) = self.details.stop.take() {
            info!("details: cancelling previous task");
            let _ = stop.send(());
        }
        // need current kind (support both curated index selection and direct GVK selection)
        let Some(kind) = self.current_selected_kind().cloned() else { return; };
        // build reference
        let reference = ResourceRef { cluster: None, gvk: kind, namespace: it.namespace.clone(), name: it.name.clone() };
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.details.stop = Some(stop_tx);
        // spawn fetch task
        self.details.task = Some(tokio::spawn(async move {
            let t0 = Instant::now();
            info!(gvk = %gvk_label(&reference.gvk), name = %reference.name, ns = %reference.namespace.as_deref().unwrap_or("-"), "details: fetch start");
            let fetch = async {
                match api.get_raw(reference).await {
                    Ok(bytes) => {
                        // Parse JSON (if possible) both for YAML rendering and for extracting pod containers
                        let (text, containers): (String, Option<Vec<String>>) = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                            Ok(v) => {
                                // Extract pod containers if applicable
                                let mut names: Vec<String> = Vec::new();
                                if v.get("kind").and_then(|k| k.as_str()).unwrap_or("") == "Pod" {
                                    if let Some(spec) = v.get("spec") {
                                        if let Some(conts) = spec.get("containers").and_then(|c| c.as_array()) {
                                            for c in conts { if let Some(n) = c.get("name").and_then(|n| n.as_str()) { names.push(n.to_string()); } }
                                        }
                                        if let Some(inits) = spec.get("initContainers").and_then(|c| c.as_array()) {
                                            for c in inits { if let Some(n) = c.get("name").and_then(|n| n.as_str()) { names.push(n.to_string()); } }
                                        }
                                        if let Some(ephs) = spec.get("ephemeralContainers").and_then(|c| c.as_array()) {
                                            for c in ephs { if let Some(n) = c.get("name").and_then(|n| n.as_str()) { names.push(n.to_string()); } }
                                        }
                                    }
                                }
                                let mut uniq = std::collections::BTreeSet::new();
                                let dedup: Vec<String> = names.into_iter().filter(|n| uniq.insert(n.clone())).collect();
                                let y = match serde_yaml::to_string(&v) { Ok(y) => y, Err(_) => String::from_utf8_lossy(&bytes).into_owned() };
                                (y, if dedup.is_empty() { None } else { Some(dedup) })
                            }
                            Err(_) => (String::from_utf8_lossy(&bytes).into_owned(), None),
                        };
                        info!(size = bytes.len(), took_ms = %t0.elapsed().as_millis(), "details: fetch ok");
                        if let Some(tx) = tx_opt.as_ref() {
                            let _ = tx.send(UiUpdate::Detail(text));
                            if let Some(v) = containers { let _ = tx.send(UiUpdate::PodContainers(v)); }
                        }
                    }
                    Err(e) => {
                        info!(took_ms = %t0.elapsed().as_millis(), error = %e, "details: fetch failed");
                        if let Some(tx) = tx_opt.as_ref() { let _ = tx.send(UiUpdate::DetailError(e.to_string())); }
                    }
                }
            };
            tokio::select! { _ = &mut stop_rx => {}, _ = fetch => {} }
        }));
    }
}
