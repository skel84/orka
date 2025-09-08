#![forbid(unsafe_code)]

use eframe::egui;
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate};
use orka_core::columns::ColumnKind;

use crate::util::render_age;

use super::OrkaGuiApp;

impl OrkaGuiApp {
    pub(crate) fn ui_results(&mut self, ui: &mut egui::Ui) {
        ui.heading("Results");
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
        let rows_len = self.results.len() as u64;
        // Build columns vector before creating the delegate to avoid borrow conflicts
        let cols_spec = self.active_cols.clone();
        let cols: Vec<Column> = if cols_spec.is_empty() {
            vec![Column::new(160.0).resizable(true), Column::new(240.0).resizable(true), Column::new(70.0).resizable(true)]
        } else {
            cols_spec.iter().map(|c| Column::new(c.width).resizable(true)).collect()
        };
        let mut delegate = ResultsDelegate { app: self };
        Table::new()
            .id_salt("results_table")
            .headers(vec![HeaderRow::new(20.0)])
            .num_rows(rows_len)
            .columns(cols)
            .show(ui, &mut delegate);
    }
}

struct ResultsDelegate<'a> {
    app: &'a mut OrkaGuiApp,
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
        if let Some(it) = self.app.results.get(idx).cloned() {
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
            let col = self.app.active_cols.get(cell.col_nr as usize);
            if let Some(spec) = col {
                match &spec.kind {
                    ColumnKind::Namespace => {
                        let ns = it.namespace.as_deref().unwrap_or("-");
                        let _ = ui.selectable_label(is_sel, egui::RichText::new(ns).monospace());
                    }
                    ColumnKind::Name => {
                        let resp = ui.selectable_label(is_sel, egui::RichText::new(&it.name).monospace());
                        if resp.clicked() { self.app.select_row(it); }
                    }
                    ColumnKind::Age => {
                        ui.label(egui::RichText::new(render_age(it.creation_ts)).monospace());
                    }
                    ColumnKind::Projected(id) => {
                        let val = it.projected.iter().find(|(k, _)| k == id).map(|(_, v)| v.as_str()).unwrap_or("-");
                        ui.label(egui::RichText::new(val).monospace());
                    }
                }
            }
        }
    }

    fn default_row_height(&self) -> f32 {
        18.0
    }
}
