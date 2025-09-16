#![forbid(unsafe_code)]

use eframe::egui;
use egui::ScrollArea;
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate, TableState};
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
            let showing = if self.results.filter.is_empty() {
                total.min(self.results.soft_cap)
            } else {
                self.compute_filtered_ix().len()
            };
            ui.label(format!("Showing {} of {}", showing, total));
            ui.separator();
            egui::ComboBox::from_label("Rows")
                .selected_text(match self.results.virtual_mode {
                    VirtualMode::Auto => "Auto",
                    VirtualMode::On => "Virtual",
                    VirtualMode::Off => "Table",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.results.virtual_mode, VirtualMode::Auto, "Auto");
                    ui.selectable_value(&mut self.results.virtual_mode, VirtualMode::On, "Virtual");
                    ui.selectable_value(&mut self.results.virtual_mode, VirtualMode::Off, "Table");
                });
            if let Some(kind) = self.current_selected_kind() {
                if kind.namespaced {
                    ui.separator();
                    ui.label("Namespace:");
                    let combo_id = egui::Id::new("results_namespace_filter");
                    let was_open = egui::ComboBox::is_open(ui.ctx(), combo_id);
                    let prev_ns = self.selection.namespace.clone();
                    let has_filter = !self.selection.namespace.is_empty();
                    let selected_label = if has_filter {
                        self.selection.namespace.clone()
                    } else {
                        "All namespaces".to_string()
                    };
                    let selected_text: egui::WidgetText = if has_filter {
                        egui::RichText::new(selected_label.clone())
                            .color(ui.visuals().warn_fg_color)
                            .into()
                    } else {
                        selected_label.clone().into()
                    };
                    egui::ComboBox::from_id_salt("results_namespace_filter")
                        .selected_text(selected_text)
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            let filter_active = !self.selection.namespace.is_empty();
                            if !self.namespaces.is_empty() {
                                let search = egui::TextEdit::singleline(
                                    &mut self.selection.namespace_filter_query,
                                )
                                .hint_text("Search namespaces…")
                                .desired_width(180.0);
                                ui.add(search);
                                ui.separator();
                            }
                            let filter = self.selection.namespace_filter_query.to_lowercase();
                            if ui
                                .selectable_label(
                                    self.selection.namespace.is_empty(),
                                    "All namespaces",
                                )
                                .clicked()
                            {
                                self.selection.namespace.clear();
                                ui.close();
                            }
                            let mut shown = 0usize;
                            for ns in &self.namespaces {
                                if !filter.is_empty() && !ns.to_lowercase().contains(&filter) {
                                    continue;
                                }
                                shown += 1;
                                if ui
                                    .selectable_label(self.selection.namespace == *ns, ns)
                                    .clicked()
                                {
                                    self.selection.namespace = ns.clone();
                                    ui.close();
                                }
                            }
                            if self.namespaces.is_empty() {
                                ui.label(
                                    egui::RichText::new("Loading namespaces…").italics().weak(),
                                );
                            } else if shown == 0 {
                                ui.label(egui::RichText::new("No matches").italics().weak());
                            }
                            if filter_active && ui.button("Clear selection").clicked() {
                                self.selection.namespace.clear();
                                ui.close();
                            }
                        });
                    let now_open = egui::ComboBox::is_open(ui.ctx(), combo_id);
                    if (was_open && !now_open) || prev_ns != self.selection.namespace {
                        self.selection.namespace_filter_query.clear();
                    }
                    if has_filter
                        && ui
                            .small_button("×")
                            .on_hover_text("Clear namespace filter")
                            .clicked()
                    {
                        self.selection.namespace.clear();
                    }
                }
            }
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
            VirtualMode::Auto => {
                self.results.filter.is_empty() && filtered_ix.len() > self.results.soft_cap
            }
        };
        if should_virtual {
            self.sync_virtual_column_widths(ui);
            self.ui_results_virtual(ui, &filtered_ix);
            return;
        }
        if self.results.filter.is_empty() && self.results.rows.len() > self.results.soft_cap {
            ui.add_space(2.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "Large result set: showing the first {} of {}. Refine filters to narrow down.",
                    self.results.soft_cap,
                    self.results.rows.len()
                ),
            );
        }
        let rows_len = filtered_ix.len() as u64;
        // Build columns vector before creating the delegate to avoid borrow conflicts
        let cols_spec = self.results.active_cols.clone();
        let cols: Vec<Column> = if cols_spec.is_empty() {
            vec![
                Column::new(160.0).resizable(true),
                Column::new(240.0).resizable(true),
                Column::new(70.0).resizable(true),
            ]
        } else {
            cols_spec
                .iter()
                .map(|c| Column::new(c.width).resizable(true))
                .collect()
        };
        if rows_len == 0 && !self.results.rows.is_empty() && !self.results.filter.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("No matches").italics().weak());
            return;
        }
        let mut delegate = ResultsDelegate {
            app: self,
            filtered_ix,
        };
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
                if h.contains(&q) {
                    out.push(i);
                    continue 'outer;
                }
            } else {
                // Fallback (rare)
                let hay = format!(
                    "{} {} {}",
                    it.name,
                    it.namespace.as_deref().unwrap_or(""),
                    it.projected
                        .iter()
                        .map(|(_, v)| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
                .to_lowercase();
                if hay.contains(&q) {
                    out.push(i);
                    continue 'outer;
                }
            }
        }
        out
    }

    fn draw_results_header(&mut self, ui: &mut egui::Ui) {
        let row_h = 20.0;
        let col_specs = self.results.active_cols.clone();
        if col_specs.is_empty() {
            return;
        }
        let width_sum: f32 = col_specs.iter().map(|c| c.width).sum();
        let row_width = width_sum.max(ui.available_width()).max(1.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(row_width, row_h), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let bg = ui.visuals().widgets.inactive.bg_fill;
        painter.rect_filled(rect, 0.0, bg);
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        let mut acc = rect.left();
        for (i, spec) in col_specs.iter().enumerate() {
            acc += spec.width;
            if i + 1 != col_specs.len() && acc < rect.right() {
                painter.line_segment(
                    [egui::pos2(acc, rect.top()), egui::pos2(acc, rect.bottom())],
                    stroke,
                );
            }
        }
        let mut header_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("results_virtual_header")
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        header_ui.set_clip_rect(rect);
        header_ui.spacing_mut().item_spacing.x = 0.0;
        for (col_idx, spec) in col_specs.iter().cloned().enumerate() {
            let label = spec.label;
            if label.is_empty() {
                continue;
            }
            let is_sorted = self.results.sort_col == Some(col_idx);
            let mut text = label.to_string();
            if is_sorted {
                text.push_str(if self.results.sort_asc {
                    " ↑"
                } else {
                    " ↓"
                });
            }
            let resp = header_ui.add_sized(
                [spec.width, row_h],
                egui::Button::new(egui::RichText::new(text).strong()).selected(is_sorted),
            );
            if resp.clicked() {
                if is_sorted {
                    self.results.sort_asc = !self.results.sort_asc;
                } else {
                    self.results.sort_col = Some(col_idx);
                    self.results.sort_asc = true;
                }
                self.results.sort_dirty = true;
            }
        }
    }

    fn ui_results_virtual(&mut self, ui: &mut egui::Ui, filtered_ix: &[usize]) {
        // Header row (clickable for sorting)
        self.draw_results_header(ui);
        ui.separator();
        let row_h = 18.0;
        let total = filtered_ix.len();
        let col_specs = self.results.active_cols.clone();
        let width_sum: f32 = col_specs.iter().map(|c| c.width).sum();
        ScrollArea::vertical()
            .id_salt("results_virtual_rows")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, total, |ui, row_range| {
                let row_width = width_sum
                    .max(ui.available_width())
                    .max(1.0);
                for row_idx in row_range {
                    let idx = filtered_ix[row_idx];
                    if let Some(it) = self.results.rows.get(idx).cloned() {
                        let is_sel =
                            self.details.selected.map(|u| u == it.uid).unwrap_or(false);
                        let is_hit = self.search.hits.contains_key(&it.uid);
                        let (row_rect, _) = ui.allocate_exact_size(
                            egui::vec2(row_width, row_h),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter_at(row_rect);
                        if is_sel {
                            painter.rect_filled(row_rect, 0.0, ui.visuals().selection.bg_fill);
                        } else if row_idx % 2 == 0 {
                            painter.rect_filled(row_rect, 0.0, ui.visuals().faint_bg_color);
                        }
                        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
                        let mut divider_x = row_rect.left();
                        for (i, spec) in col_specs.iter().enumerate() {
                            divider_x += spec.width;
                            if i + 1 != col_specs.len() && divider_x < row_rect.right() {
                                painter.line_segment(
                                    [
                                        egui::pos2(divider_x, row_rect.top()),
                                        egui::pos2(divider_x, row_rect.bottom()),
                                    ],
                                    stroke,
                                );
                            }
                        }
                        let mut row_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt(("results_virtual_row", row_idx))
                                .max_rect(row_rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );
                        row_ui.set_clip_rect(row_rect);
                        row_ui.spacing_mut().item_spacing.x = 0.0;
                        row_ui.set_min_height(row_h);
                        for (col_idx, spec) in col_specs.iter().cloned().enumerate() {
                            let mut text = self.display_cell_string(&it, col_idx, &spec);
                            if is_hit && matches!(spec.kind, ColumnKind::Name) {
                                text = format!("★ {}", text);
                            }
                            match spec.kind {
                                ColumnKind::Name | ColumnKind::Namespace => {
                                    let button = egui::Button::new(
                                        egui::RichText::new(text).monospace(),
                                    )
                                    .frame(false)
                                    .fill(egui::Color32::TRANSPARENT)
                                    .selected(is_sel);
                                    let resp = row_ui.add_sized([spec.width, row_h], button);
                                    if resp.clicked() {
                                        self.select_row(it.clone());
                                    }
                                    resp.context_menu(|ui| {
                                        if ui.button("Open Details").clicked() {
                                            self.select_row(it.clone());
                                            ui.close();
                                        }
                                        let logs_enabled = self
                                            .selected_is_pod()
                                            && self
                                                .ops
                                                .caps
                                                .as_ref()
                                                .map(|c| c.pods_log_get)
                                                .unwrap_or(false);
                                        if logs_enabled {
                                            if ui.button("Logs").clicked() {
                                                self.select_row(it.clone());
                                                self.start_logs_task();
                                                ui.close();
                                            }
                                        } else {
                                            ui.add_enabled(false, egui::Button::new("Logs"))
                                                .on_hover_text("Logs available for Pods only");
                                        }
                                        let scalable = self
                                            .ops
                                            .caps
                                            .as_ref()
                                            .and_then(|c| c.scale.as_ref())
                                            .is_some();
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
                                        if self.selected_is_pod()
                                            && ui.button("Delete…").clicked()
                                        {
                                            self.select_row(it.clone());
                                            if let Some((ns, pod)) = self.current_pod_selection() {
                                                self.ops.confirm_delete = Some((ns, pod));
                                            }
                                            ui.close();
                                        }
                                    });
                                }
                                _ => {
                                    row_ui.add_sized(
                                        [spec.width, row_h],
                                        egui::Label::new(egui::RichText::new(text).monospace()),
                                    );
                                }
                            }
                        }
                    }
                }
            });
    }

    fn sync_virtual_column_widths(&mut self, ui: &egui::Ui) {
        if self.results.active_cols.is_empty() {
            return;
        }
        let table_id = TableState::id(ui, egui::Id::new("results_table"));
        if let Some(state) = TableState::load(ui.ctx(), table_id) {
            for (idx, spec) in self.results.active_cols.iter_mut().enumerate() {
                let col_id = egui::Id::new(idx);
                if let Some(width) = state.col_widths.get(&col_id) {
                    if width.is_finite() && *width > 0.0 {
                        spec.width = *width;
                    }
                }
            }
        }
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
            let col_idx = cell.col_range.start;
            let label = self
                .app
                .results
                .active_cols
                .get(col_idx)
                .map(|c| c.label)
                .unwrap_or("");
            if !label.is_empty() {
                ui.add_space(2.0);
                let is_sorted = self.app.results.sort_col == Some(col_idx);
                let mut text = label.to_string();
                if is_sorted {
                    text.push_str(if self.app.results.sort_asc {
                        " ↑"
                    } else {
                        " ↓"
                    });
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
            let is_sel = self
                .app
                .details
                .selected
                .map(|u| u == it.uid)
                .unwrap_or(false);
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
            let col_idx = cell.col_nr;
            if let Some(spec) = self.app.results.active_cols.get(col_idx).cloned() {
                let mut text = self.app.display_cell_string(&it, col_idx, &spec);
                if is_hit && matches!(spec.kind, ColumnKind::Name) {
                    text = format!("★ {}", text);
                }
                match spec.kind {
                    ColumnKind::Name | ColumnKind::Namespace => {
                        let resp = ui.add(
                            egui::Button::new(egui::RichText::new(text).monospace())
                                .selected(is_sel),
                        );
                        if resp.clicked() {
                            self.app.select_row(it.clone());
                        }
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
                        if self.app.selected_is_pod() && ui.button("Delete…").clicked() {
                            self.app.select_row(it.clone());
                            if let Some((ns, pod)) = self.app.current_pod_selection() {
                                self.app.ops.confirm_delete = Some((ns, pod));
                            }
                            ui.close();
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
