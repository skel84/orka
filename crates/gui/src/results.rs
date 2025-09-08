#![forbid(unsafe_code)]

use eframe::egui;
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate};
use egui::ScrollArea;
use orka_core::columns::ColumnKind;

// render_age is used via app.display_cell_string for Age; no direct import here

use super::{OrkaGuiApp, VirtualMode};

impl OrkaGuiApp {
    pub(crate) fn ui_results(&mut self, ui: &mut egui::Ui) {
        ui.heading("Results");
        // Filter box: simple substring filter over name/namespace/projected values
        ui.horizontal(|ui| {
            ui.label("Filter:");
            let te = egui::TextEdit::singleline(&mut self.results.filter)
                .hint_text("name, namespace, projected…");
            ui.add(te);
            if ui.button("×").on_hover_text("Clear filter").clicked() {
                self.results.filter.clear();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.results.filter.clear();
            }
            ui.separator();
            if !self.search.hits.is_empty() {
                ui.label(format!("Hits: {}", self.search.hits.len()));
                ui.separator();
            }
            let total = self.results.rows.len();
            let showing = if self.results.filter.is_empty() { total.min(self.results.soft_cap) } else { self.compute_filtered_ix().len() };
            ui.label(format!("Showing {} of {}", showing, total));
            ui.separator();
            egui::ComboBox::from_label("Rows")
                .selected_text(match self.results.virtual_mode { VirtualMode::Auto => "Auto", VirtualMode::On => "Virtual", VirtualMode::Off => "Table" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.results.virtual_mode, VirtualMode::Auto, "Auto");
                    ui.selectable_value(&mut self.results.virtual_mode, VirtualMode::On, "Virtual");
                    ui.selectable_value(&mut self.results.virtual_mode, VirtualMode::Off, "Table");
                });
        });
        if self.results.rows.is_empty() {
            if self.last_error.is_none() && self.watch.task.is_some() {
                ui.add(egui::Spinner::new());
            } else {
                ui.label(
                    egui::RichText::new("Select a Kind to load results")
                        .italics()
                        .weak(),
                );
            }
        }
        // Apply pending sort before drawing the table
        self.apply_sort_if_needed();
        // Compute filtered index mapping for this frame
        let filtered_ix = self.compute_filtered_ix();
        // Virtualization path using ScrollArea::show_rows for large unfiltered sets
        let should_virtual = match self.results.virtual_mode {
            VirtualMode::On => true,
            VirtualMode::Off => false,
            VirtualMode::Auto => self.results.filter.is_empty() && filtered_ix.len() > self.results.soft_cap,
        };
        if should_virtual {
            self.ui_results_virtual(ui, &filtered_ix);
            return;
        }
        if self.results.filter.is_empty() && self.results.rows.len() > self.results.soft_cap {
            ui.add_space(2.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "Large result set: showing the first {} of {}. Refine filters to narrow down.",
                    self.results.soft_cap, self.results.rows.len()
                ),
            );
        }
        let rows_len = filtered_ix.len() as u64;
        // Build columns vector before creating the delegate to avoid borrow conflicts
        let cols_spec = self.results.active_cols.clone();
        let cols: Vec<Column> = if cols_spec.is_empty() {
            vec![Column::new(160.0).resizable(true), Column::new(240.0).resizable(true), Column::new(70.0).resizable(true)]
        } else {
            cols_spec.iter().map(|c| Column::new(c.width).resizable(true)).collect()
        };
        if rows_len == 0 && !self.results.rows.is_empty() && !self.results.filter.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("No matches").italics().weak());
            return;
        }
        let mut delegate = ResultsDelegate { app: self, filtered_ix };
        Table::new()
            .id_salt("results_table")
            .headers(vec![HeaderRow::new(20.0)])
            .num_rows(rows_len)
            .columns(cols)
            .show(ui, &mut delegate);
    }

    pub(crate) fn compute_filtered_ix(&self) -> Vec<usize> {
        if self.results.filter.is_empty() {
            let cap = self.results.soft_cap.min(self.results.rows.len());
            return (0..cap).collect();
        }
        let q = self.results.filter.to_lowercase();
        let mut out = Vec::with_capacity(self.results.rows.len());
        'outer: for (i, it) in self.results.rows.iter().enumerate() {
            if let Some(h) = self.results.filter_cache.get(&it.uid) {
                if h.contains(&q) { out.push(i); continue 'outer; }
            } else {
                // Fallback (rare)
                let hay = format!(
                    "{} {} {}",
                    it.name,
                    it.namespace.as_deref().unwrap_or(""),
                    it.projected.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>().join(" ")
                )
                .to_lowercase();
                if hay.contains(&q) { out.push(i); continue 'outer; }
            }
        }
        out
    }

    fn draw_results_header(&mut self, ui: &mut egui::Ui) {
        let row_h = 20.0;
        let rect = ui.max_rect();
        let bg = ui.visuals().widgets.inactive.bg_fill;
        ui.painter().rect_filled(rect, 0.0, bg);
        ui.horizontal(|ui| {
            for (col_idx, spec) in self.results.active_cols.clone().into_iter().enumerate() {
                let label = spec.label;
                if label.is_empty() { continue; }
                let is_sorted = self.results.sort_col == Some(col_idx);
                let mut text = label.to_string();
                if is_sorted { text.push_str(if self.results.sort_asc { " ↑" } else { " ↓" }); }
                let resp = ui.add_sized([spec.width, row_h], egui::Button::new(egui::RichText::new(text).strong()).selected(is_sorted));
                if resp.clicked() {
                    if is_sorted { self.results.sort_asc = !self.results.sort_asc; } else { self.results.sort_col = Some(col_idx); self.results.sort_asc = true; }
                    self.results.sort_dirty = true;
                }
            }
        });
    }

    fn ui_results_virtual(&mut self, ui: &mut egui::Ui, filtered_ix: &Vec<usize>) {
        // Header row (clickable for sorting)
        self.draw_results_header(ui);
        ui.separator();
        let row_h = 18.0;
        let total = filtered_ix.len();
        ScrollArea::vertical().show_rows(ui, row_h, total, |ui, row_range| {
            for row_idx in row_range {
                let idx = filtered_ix[row_idx];
                if let Some(it) = self.results.rows.get(idx).cloned() {
                    let is_sel = self.details.selected.map(|u| u == it.uid).unwrap_or(false);
                    let is_hit = self.search.hits.contains_key(&it.uid);
                    let rect = ui.max_rect();
                    if is_sel {
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
                    } else if row_idx % 2 == 0 {
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                    }
                    ui.horizontal(|ui| {
                        for (col_idx, spec) in self.results.active_cols.clone().into_iter().enumerate() {
                            let mut text = self.display_cell_string(&it, col_idx, &spec);
                            if is_hit && matches!(spec.kind, ColumnKind::Name) {
                                text = format!("★ {}", text);
                            }
                            match spec.kind {
                                ColumnKind::Name | ColumnKind::Namespace => {
                                    let resp = ui.add_sized([spec.width, row_h], egui::Button::new(egui::RichText::new(text).monospace()).selected(is_sel));
                                    if resp.clicked() { self.select_row(it.clone()); }
                                    // Context menu on right-click
                                    resp.context_menu(|ui| {
                                        if ui.button("Open Details").clicked() {
                                            self.select_row(it.clone());
                                            ui.close();
                                        }
                                        let logs_enabled = self.selected_is_pod() && self.ops.caps.as_ref().map(|c| c.pods_log_get).unwrap_or(false);
                                        if logs_enabled {
                                            if ui.button("Logs").clicked() {
                                                self.select_row(it.clone());
                                                self.start_logs_task();
                                                ui.close();
                                            }
                                        } else {
                                            ui.add_enabled(false, egui::Button::new("Logs")).on_hover_text("Logs available for Pods only");
                                        }
                                        // Workload ops: Rollout / Scale
                                        let scalable = self.ops.caps.as_ref().and_then(|c| c.scale.as_ref()).is_some();
                                        if scalable {
                                            if ui.button("Rollout Restart").clicked() {
                                                tracing::info!(name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "ui: rollout restart click (row menu)");
                                                self.select_row(it.clone());
                                                self.start_rollout_restart_task();
                                                ui.close();
                                            }
                                            if ui.button("Scale…").clicked() {
                                                tracing::info!(name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "ui: scale prompt open (row menu)");
                                                self.select_row(it.clone());
                                                self.ops.scale_prompt_open = true;
                                                ui.close();
                                            }
                                        }
                                        // Delete Pod (Pods only)
                                        if self.selected_is_pod() {
                                            if ui.button("Delete…").clicked() {
                                                self.select_row(it.clone());
                                                if let Some((ns, pod)) = self.current_pod_selection() {
                                                    self.ops.confirm_delete = Some((ns, pod));
                                                }
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                                _ => {
                                    ui.add_sized([spec.width, row_h], egui::Label::new(egui::RichText::new(text).monospace()));
                                }
                            }
                        }
                    });
                }
            }
        });
    }
}

struct ResultsDelegate<'a> {
    app: &'a mut OrkaGuiApp,
    filtered_ix: Vec<usize>,
}

impl<'a> TableDelegate for ResultsDelegate<'a> {
    fn prepare(&mut self, _info: &egui_table::PrefetchInfo) {}

    fn header_cell_ui(&mut self, ui: &mut egui::Ui, cell: &HeaderCellInfo) {
        if cell.row_nr == 0 {
            // Fill header cell background for contrast
            let rect = ui.max_rect();
            let bg = ui.visuals().widgets.inactive.bg_fill;
            ui.painter().rect_filled(rect, 0.0, bg);
            let col_idx = cell.col_range.start as usize;
            let label = self
                .app
                .results.active_cols
                .get(col_idx)
                .map(|c| c.label)
                .unwrap_or("");
            if !label.is_empty() {
                ui.add_space(2.0);
                let is_sorted = self.app.results.sort_col == Some(col_idx);
                let mut text = label.to_string();
                if is_sorted {
                    text.push_str(if self.app.results.sort_asc { " ↑" } else { " ↓" });
                }
                let resp = ui.selectable_label(is_sorted, egui::RichText::new(text).strong());
                if resp.clicked() {
                    if is_sorted {
                        self.app.results.sort_asc = !self.app.results.sort_asc;
                    } else {
                        self.app.results.sort_col = Some(col_idx);
                        self.app.results.sort_asc = true;
                    }
                    self.app.results.sort_dirty = true;
                }
            }
        }
    }

    fn cell_ui(&mut self, ui: &mut egui::Ui, cell: &CellInfo) {
        let idx = cell.row_nr as usize;
        let real_idx = *self.filtered_ix.get(idx).unwrap_or(&idx);
        if let Some(it) = self.app.results.rows.get(real_idx).cloned() {
            let is_sel = self.app.details.selected.map(|u| u == it.uid).unwrap_or(false);
            let is_hit = self.app.search.hits.contains_key(&it.uid);
            // zebra stripes and selection background
            let rect = ui.max_rect();
            if is_sel {
                ui.painter()
                    .rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
            } else if idx % 2 == 0 {
                ui.painter()
                    .rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
            }
            let col_idx = cell.col_nr as usize;
            if let Some(spec) = self.app.results.active_cols.get(col_idx).cloned() {
                let mut text = self.app.display_cell_string(&it, col_idx, &spec);
                if is_hit && matches!(spec.kind, ColumnKind::Name) { text = format!("★ {}", text); }
                match spec.kind {
                    ColumnKind::Name | ColumnKind::Namespace => {
                        let resp = ui.add(egui::Button::new(egui::RichText::new(text).monospace()).selected(is_sel));
                        if resp.clicked() { self.app.select_row(it.clone()); }
                        // Row context menu
                        resp.context_menu(|ui| {
                            if ui.button("Open Details").clicked() {
                                self.app.select_row(it.clone());
                                ui.close();
                            }
                            let logs_enabled = self.app.selected_is_pod() && self.app.ops.caps.as_ref().map(|c| c.pods_log_get).unwrap_or(false);
                            if logs_enabled {
                                if ui.button("Logs").clicked() {
                                    self.app.select_row(it.clone());
                                    self.app.start_logs_task();
                                    ui.close();
                                }
                            } else {
                                ui.add_enabled(false, egui::Button::new("Logs")).on_hover_text("Logs available for Pods only");
                            }
                            // Workload ops: Rollout / Scale
                            let scalable = self.app.ops.caps.as_ref().and_then(|c| c.scale.as_ref()).is_some();
                            if scalable {
                                if ui.button("Rollout Restart").clicked() {
                                    tracing::info!(name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "ui: rollout restart click (row menu)");
                                    self.app.select_row(it.clone());
                                    self.app.start_rollout_restart_task();
                                    ui.close();
                                }
                                if ui.button("Scale…").clicked() {
                                    tracing::info!(name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "ui: scale prompt open (row menu)");
                                    self.app.select_row(it.clone());
                                    self.app.ops.scale_prompt_open = true;
                                    ui.close();
                                }
                            }
                            // Delete Pod (Pods only)
                            if self.app.selected_is_pod() {
                                if ui.button("Delete…").clicked() {
                                    self.app.select_row(it.clone());
                                    if let Some((ns, pod)) = self.app.current_pod_selection() {
                                        self.app.ops.confirm_delete = Some((ns, pod));
                                    }
                                    ui.close();
                                }
                            }
                        });
                    }
                    _ => {
                        ui.label(egui::RichText::new(text).monospace());
                    }
                }
            }
        }
    }

    fn default_row_height(&self) -> f32 {
        18.0
    }
}
