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
            let te = egui::TextEdit::singleline(&mut self.results_filter)
                .hint_text("name, namespace, projected…");
            ui.add(te);
            if ui.button("×").on_hover_text("Clear filter").clicked() {
                self.results_filter.clear();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.results_filter.clear();
            }
            ui.separator();
            let total = self.results.len();
            let showing = if self.results_filter.is_empty() { total.min(self.results_soft_cap) } else { self.compute_filtered_ix().len() };
            ui.label(format!("Showing {} of {}", showing, total));
            ui.separator();
            egui::ComboBox::from_label("Rows")
                .selected_text(match self.results_virtual_mode { VirtualMode::Auto => "Auto", VirtualMode::On => "Virtual", VirtualMode::Off => "Table" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.results_virtual_mode, VirtualMode::Auto, "Auto");
                    ui.selectable_value(&mut self.results_virtual_mode, VirtualMode::On, "Virtual");
                    ui.selectable_value(&mut self.results_virtual_mode, VirtualMode::Off, "Table");
                });
        });
        if self.results.is_empty() {
            if self.last_error.is_none() && self.watch_task.is_some() {
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
        let should_virtual = match self.results_virtual_mode {
            VirtualMode::On => true,
            VirtualMode::Off => false,
            VirtualMode::Auto => self.results_filter.is_empty() && filtered_ix.len() > self.results_soft_cap,
        };
        if should_virtual {
            self.ui_results_virtual(ui, &filtered_ix);
            return;
        }
        if self.results_filter.is_empty() && self.results.len() > self.results_soft_cap {
            ui.add_space(2.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "Large result set: showing the first {} of {}. Refine filters to narrow down.",
                    self.results_soft_cap, self.results.len()
                ),
            );
        }
        let rows_len = filtered_ix.len() as u64;
        // Build columns vector before creating the delegate to avoid borrow conflicts
        let cols_spec = self.active_cols.clone();
        let cols: Vec<Column> = if cols_spec.is_empty() {
            vec![Column::new(160.0).resizable(true), Column::new(240.0).resizable(true), Column::new(70.0).resizable(true)]
        } else {
            cols_spec.iter().map(|c| Column::new(c.width).resizable(true)).collect()
        };
        if rows_len == 0 && !self.results.is_empty() && !self.results_filter.is_empty() {
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
        if self.results_filter.is_empty() {
            let cap = self.results_soft_cap.min(self.results.len());
            return (0..cap).collect();
        }
        let q = self.results_filter.to_lowercase();
        let mut out = Vec::with_capacity(self.results.len());
        'outer: for (i, it) in self.results.iter().enumerate() {
            if let Some(h) = self.filter_cache.get(&it.uid) {
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
            for (col_idx, spec) in self.active_cols.clone().into_iter().enumerate() {
                let label = spec.label;
                if label.is_empty() { continue; }
                let is_sorted = self.sort_col == Some(col_idx);
                let mut text = label.to_string();
                if is_sorted { text.push_str(if self.sort_asc { " ↑" } else { " ↓" }); }
                let resp = ui.add_sized([spec.width, row_h], egui::SelectableLabel::new(is_sorted, egui::RichText::new(text).strong()));
                if resp.clicked() {
                    if is_sorted { self.sort_asc = !self.sort_asc; } else { self.sort_col = Some(col_idx); self.sort_asc = true; }
                    self.sort_dirty = true;
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
                if let Some(it) = self.results.get(idx).cloned() {
                    let is_sel = self.selected.map(|u| u == it.uid).unwrap_or(false);
                    let rect = ui.max_rect();
                    if is_sel {
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
                    } else if row_idx % 2 == 0 {
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                    }
                    ui.horizontal(|ui| {
                        for (col_idx, spec) in self.active_cols.clone().into_iter().enumerate() {
                            let text = self.display_cell_string(&it, col_idx, &spec);
                            match spec.kind {
                                ColumnKind::Name | ColumnKind::Namespace => {
                                    let resp = ui.add_sized([spec.width, row_h], egui::SelectableLabel::new(is_sel, egui::RichText::new(text).monospace()));
                                    if resp.clicked() { self.select_row(it.clone()); }
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
                .active_cols
                .get(col_idx)
                .map(|c| c.label)
                .unwrap_or("");
            if !label.is_empty() {
                ui.add_space(2.0);
                let is_sorted = self.app.sort_col == Some(col_idx);
                let mut text = label.to_string();
                if is_sorted {
                    text.push_str(if self.app.sort_asc { " ↑" } else { " ↓" });
                }
                let resp = ui.selectable_label(is_sorted, egui::RichText::new(text).strong());
                if resp.clicked() {
                    if is_sorted {
                        self.app.sort_asc = !self.app.sort_asc;
                    } else {
                        self.app.sort_col = Some(col_idx);
                        self.app.sort_asc = true;
                    }
                    self.app.sort_dirty = true;
                }
            }
        }
    }

    fn cell_ui(&mut self, ui: &mut egui::Ui, cell: &CellInfo) {
        let idx = cell.row_nr as usize;
        let real_idx = *self.filtered_ix.get(idx).unwrap_or(&idx);
        if let Some(it) = self.app.results.get(real_idx).cloned() {
            let is_sel = self.app.selected.map(|u| u == it.uid).unwrap_or(false);
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
            if let Some(spec) = self.app.active_cols.get(col_idx).cloned() {
                let text = self.app.display_cell_string(&it, col_idx, &spec);
                match spec.kind {
                    ColumnKind::Name | ColumnKind::Namespace => {
                        let resp = ui.selectable_label(is_sel, egui::RichText::new(text).monospace());
                        if resp.clicked() { self.app.select_row(it); }
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
