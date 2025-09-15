#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;
use tracing::info;
use metrics::histogram;
use base64::Engine as _;

use orka_api::ResourceRef;
use orka_core::LiteObj;

use crate::util::gvk_label;
use super::{OrkaGuiApp, UiUpdate};
use crate::model::DetailsPaneTab;
use crate::model::{GraphViewMode, GraphNodeRole};

impl OrkaGuiApp {
    fn ui_edit(&mut self, ui: &mut egui::Ui) {
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
        // Secret panel (if current selection is a Secret)
        let is_secret = self
            .current_selected_kind()
            .map(|k| k.group.is_empty() && k.version == "v1" && k.kind == "Secret")
            .unwrap_or(false);
        if is_secret {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong("Secret data");
                    ui.label(egui::RichText::new("(values redacted in YAML)").weak());
                });
                ui.add_space(4.0);
                if self.details.secret_entries.is_empty() {
                    ui.label(egui::RichText::new("no data keys").weak());
                } else {
                    for entry in self.details.secret_entries.clone() {
                        ui.horizontal(|ui| {
                            ui.monospace(&entry.key);
                            ui.separator();
                            let revealed = self.details.secret_revealed.contains(&entry.key);
                            if revealed {
                                if ui.button("Hide").on_hover_text("Hide value").clicked() {
                                    self.details.secret_revealed.remove(&entry.key);
                                }
                            } else {
                                if ui.button("Reveal").on_hover_text("Reveal value").clicked() {
                                    self.details.secret_revealed.insert(entry.key.clone());
                                }
                            }
                            if ui.button("Copy").on_hover_text("Copy value to clipboard").clicked() {
                                let copy_text = match &entry.decoded {
                                    Some(s) => s.clone(),
                                    None => entry.b64.clone(),
                                };
                                ui.output_mut(|o| o.copied_text = copy_text);
                            }
                        });
                        let revealed = self.details.secret_revealed.contains(&entry.key);
                        let display = if revealed {
                            if let Some(s) = &entry.decoded { s.clone() } else { format!("(binary) base64={}", entry.b64) }
                        } else {
                            "••••••".to_string()
                        };
                        let mut tmp = display;
                        let widget = egui::TextEdit::singleline(&mut tmp)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(400.0)
                            .interactive(false);
                        ui.add(widget);
                        ui.add_space(4.0);
                    }
                }
            });
            ui.add_space(6.0);
        }
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
    }

    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        if !self.selected_is_pod() { return; }
        // Show logs content directly without a collapsing header
                // Controls
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.logs.follow, "Follow");
                    ui.separator();
                    ui.label("Wrap:");
                    ui.checkbox(&mut self.logs.wrap, "");
                    ui.separator();
                    ui.label("Colorize:");
                    ui.checkbox(&mut self.logs.colorize, "");
                    // Prefix theme mapping
                    ui.separator();
                    ui.label("Prefix:");
                    let mut theme = self.logs.prefix_theme;
                    egui::ComboBox::from_id_salt("logs_prefix_theme")
                        .selected_text(match theme { crate::model::PrefixTheme::Bright => "Bright", crate::model::PrefixTheme::Basic => "Basic", crate::model::PrefixTheme::Gray => "Gray", crate::model::PrefixTheme::None => "None" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::Bright, "Bright");
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::Basic, "Basic");
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::Gray, "Gray");
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::None, "None");
                        });
                    if theme != self.logs.prefix_theme { self.logs.prefix_theme = theme; if self.logs.running && matches!(self.logs.container.as_deref(), Some("(all)")) { self.start_logs_task(); } }
                    ui.separator();
                    ui.label("Visible:");
                    let mut vis = self.logs.visible_follow_limit as i32;
                    if ui.add(egui::DragValue::new(&mut vis).range(100..=self.logs.ring_cap as i32)).on_hover_text("Max visible lines when following").changed() {
                        self.logs.visible_follow_limit = vis.max(100) as usize;
                    }
                    ui.separator();
                    ui.label("Since(s):");
                    let mut since = self.logs.since_seconds.unwrap_or(0);
                    if ui.add(egui::DragValue::new(&mut since).range(0..=86_400)).on_hover_text("Only show lines newer than N seconds; 0 disables").changed() {
                        self.logs.since_seconds = if since <= 0 { None } else { Some(since as i64) };
                        if self.logs.running { self.start_logs_task(); }
                    }
                    ui.separator();
                    ui.label("Tail:");
                    let mut tail = self.logs.tail_lines.unwrap_or(0);
                    if ui.add(egui::DragValue::new(&mut tail).range(0..=10000)).on_hover_text("Tail last N lines; 0 disables").changed() {
                        self.logs.tail_lines = if tail <= 0 { None } else { Some(tail as i64) };
                        if self.logs.running { self.start_logs_task(); }
                    }
                    ui.separator();
                    ui.label("Container:");
                    if self.logs.containers.is_empty() {
                        ui.label(egui::RichText::new("(none)").weak());
                    } else {
                        let current = self.logs.container.clone().unwrap_or_else(|| self.logs.containers.get(0).cloned().unwrap_or_default());
                        let mut selected = current.clone();
                        let mut changed_container = false;
                        egui::ComboBox::from_id_salt("logs_container_select")
                            .selected_text(selected.clone())
                            .show_ui(ui, |ui| {
                                // Aggregated: All containers option
                                if ui.selectable_value(&mut selected, "(all)".to_string(), "(all)").changed() { changed_container = true; }
                                for name in &self.logs.containers {
                                    if ui.selectable_value(&mut selected, name.clone(), name).changed() { changed_container = true; }
                                }
                            });
                        if selected != current {
                            self.logs.container = Some(selected);
                            if self.logs.running && changed_container { self.start_logs_task(); }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Grep:");
                    let resp = ui.add(egui::TextEdit::singleline(&mut self.logs.grep).desired_width(200.0));
                    if resp.changed() {
                        // Recompile regex on change
                        let text = self.logs.grep.trim().to_string();
                        if text.is_empty() {
                            self.logs.grep_cache = None;
                            self.logs.grep_error = None;
                        } else if let Ok(re) = regex::Regex::new(&text) {
                            self.logs.grep_cache = Some((text, re));
                            self.logs.grep_error = None;
                        } else {
                            self.logs.grep_cache = None;
                            self.logs.grep_error = Some("invalid regex".into());
                        }
                    }
                    if let Some(err) = &self.logs.grep_error { ui.colored_label(ui.visuals().warn_fg_color, err); }
                    if self.logs.dropped > 0 {
                        ui.separator();
                        ui.colored_label(ui.visuals().error_fg_color, format!("dropped: {}", self.logs.dropped));
                    }
                    if ui.button("Clear").on_hover_text("Clear backlog").clicked() {
                        self.logs.ring.clear();
                        self.logs.backlog.clear();
                    }
                    if !self.logs.running {
                        if ui.button("Start").on_hover_text("Start streaming logs").clicked() { self.start_logs_task(); }
                    } else {
                        if ui.button("Stop").on_hover_text("Stop logs").clicked() { self.stop_logs_task(); }
                    }
                });
                ui.add_space(4.0);

                // Removed fallback path: always use v2 fixed-row renderer for consistency and highlighting

                // Build visible indices with grep filter and follow/paused logic
                let mut indices: Vec<usize> = if let Some((_text, re)) = &self.logs.grep_cache {
                    self.logs.ring.iter().enumerate().filter_map(|(i, p)| if re.is_match(&p.raw) { Some(i) } else { None }).collect()
                } else {
                    (0..self.logs.ring.len()).collect()
                };

                if !self.logs.follow && self.logs.order_by_ts_when_paused {
                    indices.sort_by(|&a, &b| {
                        let ta = self.logs.ring.get(a).and_then(|p| p.timestamp.clone());
                        let tb = self.logs.ring.get(b).and_then(|p| p.timestamp.clone());
                        match (ta, tb) {
                            (Some(x), Some(y)) => x.cmp(&y),
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                            (None, None) => a.cmp(&b),
                        }
                    });
                }

                if self.logs.follow {
                    let len = indices.len();
                    let take = self.logs.visible_follow_limit.min(len);
                    indices = indices.split_off(len - take);
                }

                let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                let rows = indices.len();
                let paint_start = std::time::Instant::now();
                egui::ScrollArea::vertical()
                    .id_salt("logs_scroll")
                    .stick_to_bottom(self.logs.follow)
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, if self.logs.follow { rows + self.logs.follow_pad_rows } else { rows }, |ui, range| {
                        for local_row in range.clone() {
                            // Add bottom pad rows when following
                            if self.logs.follow && local_row >= rows {
                                ui.add_space(row_h);
                                continue;
                            }
                            if let Some(&ring_idx) = indices.get(local_row) {
                                if let Some(p) = self.logs.ring.get(ring_idx) {
                                    // Fixed height row: render one line
                                    let widget = if self.logs.wrap {
                                        egui::Label::new(p.job.clone()).wrap()
                                    } else {
                                        egui::Label::new(p.job.clone()).truncate()
                                    };
                                    ui.add(widget);
                                }
                            }
                        }
                    });
                let ms = paint_start.elapsed().as_millis() as f64;
                histogram!("ui_logs_paint_ms", ms);
                histogram!("ui_logs_rows_per_paint", rows as f64);
                if !self.logs.follow && self.logs.order_by_ts_when_paused {
                    ui.colored_label(ui.visuals().weak_text_color(), "sorted by timestamp");
                }
    }
    pub(crate) fn ui_details(&mut self, ui: &mut egui::Ui) {
        ui.heading("Details");
        // Detach/Reattach controls
        ui.horizontal(|ui| {
            if let Some(uid) = self.details.selected {
                let id = egui::ViewportId::from_hash_of(("orka_details", uid));
                let is_detached = self.detached.iter().any(|w| w.meta.id == id);
                if is_detached {
                    if ui.small_button("Reattach").on_hover_text("Close window and re-open as a tab").clicked() {
                        self.open_details_tab_for(uid);
                        ui.ctx().send_viewport_cmd_to(id, egui::ViewportCommand::Close);
                        self.detached.retain(|w| w.meta.id != id);
                    }
                } else {
                    if ui.small_button("Detach to Window").on_hover_text("Open this Details view in a separate OS window").clicked() {
                        let ctx = ui.ctx();
                        self.open_detached_for(ctx, uid);
                        // Queue closing the corresponding dock tab, if any
                        self.dock_close_pending.push(uid);
                    }
                }
            } else {
                if ui.small_button("Detach to Window").clicked() {
                    self.toast("details: select a row first", crate::model::ToastKind::Info);
                }
            }
        });
        // Tab bar inside the Details pane (Edit | Logs | Svc Logs | Exec | Describe | Graph)
        ui.horizontal(|ui| {
            let tab = self.details.active_tab;
            let is_svc = self.selected_is_service();
            if ui.selectable_label(matches!(tab, DetailsPaneTab::Edit), "Edit").clicked() { self.details.active_tab = DetailsPaneTab::Edit; }
            if ui.selectable_label(matches!(tab, DetailsPaneTab::Logs), "Logs").clicked() { self.details.active_tab = DetailsPaneTab::Logs; }
            if is_svc { if ui.selectable_label(matches!(tab, DetailsPaneTab::SvcLogs), "Svc Logs").clicked() { self.details.active_tab = DetailsPaneTab::SvcLogs; } }
            if self.selected_is_pod() { if ui.selectable_label(matches!(tab, DetailsPaneTab::Exec), "Exec").clicked() { self.details.active_tab = DetailsPaneTab::Exec; } }
            if ui.selectable_label(matches!(tab, DetailsPaneTab::Describe), "Describe").clicked() { self.details.active_tab = DetailsPaneTab::Describe; }
            if ui.selectable_label(matches!(tab, DetailsPaneTab::Graph), "Graph").on_hover_text("List-based relationships").clicked() { self.details.active_tab = DetailsPaneTab::Graph; }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("details_scroll")
            .show(ui, |ui| {
                // Contextual actions bar (logs, exec, pf, scale, etc.)
                crate::ui::actions::ui_actions_bar(self, ui);
                ui.separator();
                match self.details.active_tab {
                    DetailsPaneTab::Edit => {
                        self.ui_edit(ui);
                        if self.details.buffer.is_empty() && self.edit.buffer.is_empty() {
                            ui.label("Select a row to view details");
                        }
                    }
                    DetailsPaneTab::Logs => {
                        self.ui_logs(ui);
                    }
                    DetailsPaneTab::SvcLogs => {
                        self.ui_service_logs(ui);
                    }
                    DetailsPaneTab::Exec => {
                        self.ui_exec(ui);
                    }
                    DetailsPaneTab::Describe => {
                        ui.horizontal(|ui| {
                            if ui.small_button("Refresh").clicked() {
                                if let Some(uid) = self.details.selected { self.start_describe_task(uid); }
                            }
                            if self.describe.running { ui.add(egui::Spinner::new()); }
                            if let Some(err) = &self.describe.error { ui.colored_label(ui.visuals().error_fg_color, err); }
                        });
                        ui.add_space(4.0);
                        // Auto-fetch on first open or when selection changed
                        if self.details.selected.is_some() {
                            let need_fetch = match (self.describe.uid, self.details.selected) { (Some(u0), Some(u1)) => u0 != u1, _ => true };
                            if need_fetch && !self.describe.running { if let Some(uid) = self.details.selected { self.start_describe_task(uid); } }
                        }
                        let mut text = if self.describe.text.is_empty() { String::from("(no output yet)") } else { self.describe.text.clone() };
                        let te = egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(24)
                            .desired_width(f32::INFINITY)
                            .interactive(false);
                        ui.add(te);
                    }
                    DetailsPaneTab::Graph => {
                        self.ui_graph(ui);
                    }
                }
            });
    }

    fn ui_graph(&mut self, ui: &mut egui::Ui) {
        // Trigger fetch if first open or selection changed
        if let Some(sel) = self.details.selected {
            let need_fetch = match (self.graph.uid, self.details.selected) { (Some(u0), Some(u1)) => u0 != u1, _ => true };
            if need_fetch && !self.graph.running {
                self.graph.atlas_zoom = 1.0;
                self.graph.atlas_pan = egui::vec2(0.0, 0.0);
                self.graph.details_fit_for = None;
                self.start_graph_task(sel);
            }
        }
        ui.horizontal(|ui| {
            if ui.small_button("Refresh").clicked() {
                if let Some(uid) = self.details.selected {
                    self.graph.atlas_zoom = 1.0;
                    self.graph.atlas_pan = egui::vec2(0.0, 0.0);
                    self.graph.details_fit_for = None;
                    self.start_graph_task(uid);
                }
            }
            if self.graph.running { ui.add(egui::Spinner::new()); }
            if let Some(err) = &self.graph.error { ui.colored_label(ui.visuals().error_fg_color, err); }
            ui.separator();
            ui.label("Mode:");
            ui.selectable_value(&mut self.graph.mode, GraphViewMode::Classic, "Classic");
            if self.atlas_enabled {
                ui.selectable_value(&mut self.graph.mode, GraphViewMode::Atlas, "Atlas");
            } else {
                self.graph.mode = GraphViewMode::Classic;
            }
        });
        ui.add_space(4.0);
        match self.graph.mode {
            GraphViewMode::Classic => {
                let mut text = if self.graph.text.is_empty() { String::from("(no graph yet)") } else { self.graph.text.clone() };
                let te = egui::TextEdit::multiline(&mut text)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(24)
                    .desired_width(f32::INFINITY)
                    .interactive(false);
                ui.add(te);
            }
            GraphViewMode::Atlas => {
                self.ui_graph_atlas(ui);
            }
        }
    }

    fn ui_graph_atlas(&mut self, ui: &mut egui::Ui) {
        // Controls
        ui.horizontal(|ui| {
            if ui.small_button("-").on_hover_text("Zoom out").clicked() { self.graph.atlas_zoom = (self.graph.atlas_zoom * 0.9).max(0.25); }
            if ui.small_button("+").on_hover_text("Zoom in").clicked() { self.graph.atlas_zoom = (self.graph.atlas_zoom * 1.1).min(4.0); }
            if ui.small_button("Reset").on_hover_text("Reset view").clicked() { self.graph.atlas_zoom = 1.0; self.graph.atlas_pan = egui::vec2(0.0, 0.0); }
            if ui.small_button("Fit").on_hover_text("Fit to root and neighbors").clicked() { self.graph.details_fit_for = None; }
            ui.add(egui::Slider::new(&mut self.graph.atlas_zoom, 0.25..=4.0).text("Zoom"));
            ui.label("Drag background to pan");
        });
        ui.add_space(6.0);

        // Drawing area
        let desired = egui::vec2(ui.available_width(), ui.available_height().max(260.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::drag());
        if response.dragged() {
            let d = response.drag_delta();
            self.graph.atlas_pan += d;
        }
        let painter = ui.painter_at(rect);

        // Background
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

        let Some(model) = self.graph.model.clone() else {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "(building atlas model…)", egui::TextStyle::Body.resolve(ui.style()), ui.visuals().weak_text_color());
            return;
        };
        // Group related nodes by kind for progressive disclosure
        use std::collections::{BTreeMap, HashMap};
        let mut positions: HashMap<String, egui::Pos2> = HashMap::new();
        let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new(); // kind -> Vec<(id,label)>
        let mut owner_chain: Vec<(String, usize, String)> = Vec::new(); // (id, depth, label)
        let mut root_id: Option<(String, String)> = None; // (id,label)
        for node in &model.nodes {
            match &node.role {
                GraphNodeRole::Root => { root_id = Some((node.id.clone(), node.label.clone())); }
                GraphNodeRole::OwnerChain(depth) => { owner_chain.push((node.id.clone(), *depth, node.label.clone())); }
                GraphNodeRole::Related(_t) => {
                    let entry = groups.entry(node.kind.clone()).or_default();
                    entry.push((node.id.clone(), node.label.clone()));
                }
            }
        }
        owner_chain.sort_by_key(|(_, d, _)| *d);

        if let Some((rid, _)) = &root_id { positions.insert(rid.clone(), egui::pos2(0.0, 0.0)); }
        for (id, depth, _label) in &owner_chain {
            let d = *depth as f32; positions.insert(id.clone(), egui::pos2(0.0, -d * 120.0));
        }

        // Place related group placeholders on a ring around the root.
        let mut group_pos: BTreeMap<String, egui::Pos2> = BTreeMap::new();
        let gcount = groups.len().max(1);
        // Reserve a gap at the top for the owner chain (about 60 degrees)
        let gap = std::f32::consts::PI / 3.0; // 60°
        let span = std::f32::consts::TAU - gap;
        // Start angle to the right of the root
        let start = gap * 0.5;
        let step = span / gcount as f32;
        let radius = 180.0f32; // distance from root
        for (i, (kind, _items)) in groups.keys().cloned().zip(groups.values()).enumerate() {
            let a = start + i as f32 * step;
            let x = radius * a.cos();
            let y = radius * a.sin();
            group_pos.insert(kind, egui::pos2(x, y));
        }

        // Transform logical -> screen
        let center = rect.center() + self.graph.atlas_pan;
        let z = self.graph.atlas_zoom;
        let to_screen = |p: egui::Pos2| egui::pos2(center.x + p.x * z, center.y + p.y * z);

        // One-shot auto-fit to keep root + immediate neighbors visible
        if let Some(uid) = self.graph.uid {
            if self.graph.details_fit_for != Some(uid) {
                // compute world bounds across root, owner chain, and group placeholders
                let mut min_x = 0.0f32; let mut max_x = 0.0f32; let mut min_y = 0.0f32; let mut max_y = 0.0f32;
                let mut first = true;
                let mut consider = Vec::new();
                if let Some((rid, _)) = &root_id { if let Some(p) = positions.get(rid) { consider.push(*p); } }
                for (_id, _d, _l) in &owner_chain { if let Some(p) = positions.get(_id.as_str()) { consider.push(*p); } }
                for (_k, pos) in &group_pos { consider.push(*pos); }
                for p in consider { if first { min_x = p.x; max_x = p.x; min_y = p.y; max_y = p.y; first = false; } else { min_x = min_x.min(p.x); max_x = max_x.max(p.x); min_y = min_y.min(p.y); max_y = max_y.max(p.y); } }
                if !first {
                    let pad = 80.0;
                    let world_w = (max_x - min_x) + pad * 2.0;
                    let world_h = (max_y - min_y) + pad * 2.0;
                    let screen_w = rect.width().max(1.0);
                    let screen_h = rect.height().max(1.0);
                    let mut zz = (screen_w / world_w).min(screen_h / world_h);
                    zz = zz.clamp(0.4, 2.0);
                    let cx = (min_x + max_x) * 0.5;
                    let cy = (min_y + max_y) * 0.5;
                    self.graph.atlas_zoom = zz;
                    self.graph.atlas_pan = egui::vec2(-cx * zz, -cy * zz);
                    self.graph.details_fit_for = Some(uid);
                }
            }
        }

        // Draw root + owner chain nodes and edges between them
        for (i, (id, _depth, label)) in owner_chain.iter().enumerate() {
            let a = if i == 0 { &root_id.as_ref().unwrap().0 } else { &owner_chain[i-1].0 };
            if let (Some(pa), Some(pb)) = (positions.get(a), positions.get(id)) {
                painter.line_segment([to_screen(*pa), to_screen(*pb)], egui::Stroke::new(2.0, ui.visuals().weak_text_color()));
            }
            if let Some(lp) = positions.get(id).copied() {
                let p = to_screen(lp);
                painter.circle_filled(p, 20.0 * z.clamp(0.5,1.5), ui.visuals().widgets.inactive.bg_fill);
                painter.circle_stroke(p, 20.0 * z.clamp(0.5,1.5), egui::Stroke::new(2.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
                painter.text(p, egui::Align2::CENTER_CENTER, label, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().text_color());
            }
        }
        if let Some((rid, rlabel)) = &root_id {
            if let Some(lp) = positions.get(rid).copied() {
                let p = to_screen(lp);
                painter.circle_filled(p, 22.0 * z.clamp(0.5,1.5), ui.visuals().widgets.inactive.bg_fill);
                painter.circle_stroke(p, 22.0 * z.clamp(0.5,1.5), egui::Stroke::new(2.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
                painter.text(p, egui::Align2::CENTER_CENTER, rlabel, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().text_color());
            }
        }

        // Draw related groups and link them to the root for visual continuity
        for (kind, items) in groups {
            let base = group_pos.get(&kind).copied().unwrap_or(egui::pos2(180.0, 0.0));
            let p = to_screen(base);
            if let Some((rid, _)) = &root_id { if let Some(rp) = positions.get(rid) { painter.line_segment([to_screen(*rp), p], egui::Stroke::new(1.5, ui.visuals().weak_text_color())); } }
            let color = match kind.as_str() {
                "Pods" => egui::Color32::from_rgb(86, 204, 149),
                "ServiceAccount" => egui::Color32::from_rgb(86, 156, 214),
                "ConfigMap" => egui::Color32::from_rgb(255, 206, 86),
                "Secret" => egui::Color32::from_rgb(209, 99, 196),
                "Service" => egui::Color32::from_rgb(255, 159, 64),
                _ => ui.visuals().hyperlink_color,
            };
            painter.circle_filled(p, 18.0 * z.clamp(0.5,1.3), color.gamma_multiply(0.9));
            painter.circle_stroke(p, 18.0 * z.clamp(0.5,1.3), egui::Stroke::new(2.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
            let text = format!("{} ({})", kind, items.len());
            painter.text(p + egui::vec2(0.0, 24.0), egui::Align2::CENTER_TOP, text, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().strong_text_color());
            // toggle expand on click
            let id = ui.make_persistent_id(("details_kind", &kind));
            let hit = egui::Rect::from_center_size(p, egui::vec2(140.0, 40.0));
            let resp = ui.interact(hit, id, egui::Sense::click());
            if resp.hovered() { ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand); }
            if resp.clicked() {
                if self.graph.details_expanded_kinds.contains(&kind) { self.graph.details_expanded_kinds.remove(&kind); } else { self.graph.details_expanded_kinds.insert(kind.clone()); }
            }
            if self.graph.details_expanded_kinds.contains(&kind) {
                let mut shown = 0usize;
                for (j, (id, label)) in items.iter().enumerate().take(8) {
                    let p2 = to_screen(egui::pos2(base.x, base.y + 36.0 + j as f32 * 20.0));
                    let rect = egui::Rect::from_center_size(p2, egui::vec2(180.0, 18.0));
                    let id_w = ui.make_persistent_id(("details_item", &kind, j));
                    let resp = ui.interact(rect, id_w, egui::Sense::click());
                    if resp.hovered() { ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand); }
                    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color.gamma_multiply(0.6));
                    painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
                    painter.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().text_color());
                    if resp.clicked() {
                        // Determine target (kind, ns, name) from item id formats used in GraphModel
                        let mut target: Option<(orka_api::ResourceKind, String, String)> = None;
                        let ns_cur = self
                            .details
                            .selected
                            .and_then(|u| self.results.index.get(&u).copied())
                            .and_then(|i| self.results.rows.get(i))
                            .and_then(|r| r.namespace.clone())
                            .unwrap_or_default();
                        if let Some(rest) = id.strip_prefix("cm:") {
                            target = Some((orka_api::ResourceKind { group: String::new(), version: "v1".into(), kind: "ConfigMap".into(), namespaced: true }, ns_cur.clone(), rest.to_string()));
                        } else if let Some(rest) = id.strip_prefix("sec:") {
                            target = Some((orka_api::ResourceKind { group: String::new(), version: "v1".into(), kind: "Secret".into(), namespaced: true }, ns_cur.clone(), rest.to_string()));
                        } else if let Some(rest) = id.strip_prefix("sa:") {
                            let parts: Vec<&str> = rest.split(':').collect();
                            if parts.len() == 3 {
                                let (ns0, ver, name) = (parts[0], parts[1], parts[2]);
                                target = Some((orka_api::ResourceKind { group: String::new(), version: ver.into(), kind: "ServiceAccount".into(), namespaced: true }, ns0.to_string(), name.to_string()));
                            }
                        }
                        if let Some((rk, ns, name)) = target {
                            // Switch Kind/NS and request opening the Details once rows load
                            self.selection.selected_kind = Some(rk.clone());
                            self.selection.selected_idx = None;
                            self.selection.namespace = ns.clone();
                            self.graph.pending_open = Some((rk, ns, name));
                        }
                    }
                    shown += 1;
                }
                if items.len() > shown {
                    let more = format!("+{} more", items.len() - shown);
                    let p3 = to_screen(egui::pos2(base.x, base.y + 36.0 + shown as f32 * 20.0));
                    painter.text(p3, egui::Align2::CENTER_CENTER, more, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().weak_text_color());
                }
            }
        }
    }

    fn ui_exec(&mut self, ui: &mut egui::Ui) {
        if !self.selected_is_pod() { return; }
        // Show exec content directly without a collapsing header
        // Controls
        ui.horizontal(|ui| {
                    ui.checkbox(&mut self.exec.mode_oneshot, "One-shot");
                    ui.separator();
                    ui.checkbox(&mut self.exec.pty, "PTY");
                    ui.separator();
                    ui.label("Command:");
                    ui.add(egui::TextEdit::singleline(&mut self.exec.cmd).desired_width(220.0));
                    ui.separator();
                    ui.label("Container:");
                    if self.logs.containers.is_empty() {
                        ui.label(egui::RichText::new("(none)").weak());
                    } else {
                        let current = self.exec.container.clone().unwrap_or_else(|| self.logs.containers.get(0).cloned().unwrap_or_default());
                        let mut selected = current.clone();
                        let mut changed_container = false;
                        egui::ComboBox::from_id_salt("exec_container_select")
                            .selected_text(selected.clone())
                            .show_ui(ui, |ui| {
                                for name in &self.logs.containers { if ui.selectable_value(&mut selected, name.clone(), name).changed() { changed_container = true; } }
                            });
                        if selected != current { self.exec.container = Some(selected); if self.exec.running && changed_container { self.stop_exec_task(); } }
                    }
                    if !self.exec.running {
                        if self.exec.mode_oneshot {
                            if ui.button("Run").clicked() { self.start_exec_oneshot_task(); }
                        } else {
                            if ui.button("Start").clicked() { self.start_exec_task(); }
                        }
                    } else {
                        if ui.button("Stop").clicked() { self.stop_exec_task(); }
                    }
                });

        ui.add_space(4.0);

                // Output area: use UiTerminal and compute rows/cols for PTY resize
                let mut sent_resize = false;
                if self.exec.mode_oneshot {
                    // Render captured output (simple text view)
                    let mut display = String::new();
                    let mut shown = 0usize;
                    let max_lines: usize = 5000;
                    for line in self.exec.backlog.iter().rev() {
                        display.push_str(line);
                        if !line.ends_with('\n') { display.push('\n'); }
                        shown += 1;
                        if shown >= max_lines { break; }
                    }
                    let display = if shown == 0 { String::new() } else { display.lines().rev().collect::<Vec<_>>().join("\n") };
                    let mut binding = display;
                    let te = egui::TextEdit::multiline(&mut binding)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(20)
                        .desired_width(f32::INFINITY)
                        .interactive(false);
                    ui.add(te);
                } else {
                    if self.exec.term.is_none() { self.exec.term = Some(crate::ui::term::UiTerminal::new()); }
                    if let Some(term) = self.exec.term.as_mut() {
                        let (cols, rows, is_focused) = term.ui(ui);
                        // Update focus based on widget focus; allow Esc to unfocus
                        self.exec.focused = is_focused && !ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if let Some(tx) = self.exec.resize.clone() {
                            if self.exec.last_cols != Some(cols) || self.exec.last_rows != Some(rows) {
                                let _ = tx.try_send((cols, rows));
                                self.exec.last_cols = Some(cols);
                                self.exec.last_rows = Some(rows);
                                sent_resize = true;
                            }
                        }
                    }
                }

        ui.add_space(4.0);

                // Input send line (basic) — only for interactive mode
                if !self.exec.mode_oneshot {
                    ui.horizontal(|ui| {
                        let send_btn = ui.add_enabled(self.exec.running, egui::Button::new("Send"));
                        let edit = ui.add_enabled(self.exec.running, egui::TextEdit::singleline(&mut self.exec.stdin_buf).desired_width(400.0));
                        let want_send = send_btn.clicked() || (self.exec.running && edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if want_send {
                            if let (Some(tx), s) = (self.exec.input.clone(), self.exec.stdin_buf.clone()) {
                                let mut bytes = s.into_bytes(); bytes.push(b'\n');
                                let _ = tx.try_send(bytes);
                                self.exec.stdin_buf.clear();
                            }
                        }
                    });
                }
        if self.exec.dropped > 0 { ui.colored_label(ui.visuals().warn_fg_color, format!("dropped: {}", self.exec.dropped)); }
        if sent_resize { ui.colored_label(ui.visuals().weak_text_color(), "resized"); }
        let hint = if self.exec.mode_oneshot {
            if self.exec.running { "running…" } else { "enter a command and click Run" }
        } else if self.exec.running {
            if self.exec.focused { "typing active • Esc to release" } else { "click inside to type" }
        } else { "press Start to open a shell" };
        ui.colored_label(ui.visuals().weak_text_color(), hint);

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("External term:");
            ui.add(egui::TextEdit::singleline(&mut self.exec.external_cmd).desired_width(160.0));
            if ui.button("Browse…").clicked() {
                if let Some(path) = rfd::FileDialog::new().set_title("Choose terminal app or binary").pick_file() {
                    self.exec.external_cmd = path.display().to_string();
                }
            }
            if ui.button("Open External").on_hover_text("Launch configured terminal with kubectl exec -it").clicked() { self.open_external_exec(); }
        });
        // Key input mapping when focused
        if self.exec.focused && self.exec.running {
            let mut to_send: Vec<u8> = Vec::new();
            ui.input(|i| {
                for ev in &i.events {
                    match ev {
                        egui::Event::Text(s) => { to_send.extend_from_slice(s.as_bytes()); }
                        egui::Event::Key{ key, pressed: true, modifiers, .. } => {
                            match key {
                                egui::Key::Enter => to_send.push(b'\r'),
                                egui::Key::Tab => to_send.push(b'\t'),
                                egui::Key::Backspace => to_send.push(0x7f),
                                egui::Key::ArrowLeft => to_send.extend_from_slice(b"\x1b[D"),
                                egui::Key::ArrowRight => to_send.extend_from_slice(b"\x1b[C"),
                                egui::Key::ArrowUp => to_send.extend_from_slice(b"\x1b[A"),
                                egui::Key::ArrowDown => to_send.extend_from_slice(b"\x1b[B"),
                                egui::Key::Home => to_send.extend_from_slice(b"\x1b[H"),
                                egui::Key::End => to_send.extend_from_slice(b"\x1b[F"),
                                egui::Key::PageUp => to_send.extend_from_slice(b"\x1b[5~"),
                                egui::Key::PageDown => to_send.extend_from_slice(b"\x1b[6~"),
                                egui::Key::C if modifiers.ctrl => to_send.push(0x03),
                                egui::Key::Z if modifiers.ctrl => to_send.push(0x1a),
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            });
            if !to_send.is_empty() {
                if let Some(tx) = self.exec.input.clone() { let _ = tx.try_send(to_send); }
            }
        }
    }

    fn ui_service_logs(&mut self, ui: &mut egui::Ui) {
        if !self.selected_is_service() { return; }
        // Show service logs content directly without a collapsing header
        // Controls
        ui.horizontal(|ui| {
                    ui.checkbox(&mut self.svc_logs.follow, "Follow");
                    ui.separator();
                    ui.label("Visible:");
                    let mut vis = self.svc_logs.visible_follow_limit as i32;
                    if ui.add(egui::DragValue::new(&mut vis).range(100..=self.svc_logs.ring_cap as i32)).on_hover_text("Max visible lines when following").changed() {
                        self.svc_logs.visible_follow_limit = vis.max(100) as usize;
                    }
                    ui.separator();
                    // Prefix theme selector
                    ui.label("Prefix:");
                    let mut theme = self.svc_logs.prefix_theme;
                    egui::ComboBox::from_id_salt("svc_logs_prefix_theme")
                        .selected_text(match theme { crate::model::PrefixTheme::Bright => "Bright", crate::model::PrefixTheme::Basic => "Basic", crate::model::PrefixTheme::Gray => "Gray", crate::model::PrefixTheme::None => "None" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::Bright, "Bright");
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::Basic, "Basic");
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::Gray, "Gray");
                            ui.selectable_value(&mut theme, crate::model::PrefixTheme::None, "None");
                        });
                    if theme != self.svc_logs.prefix_theme { self.svc_logs.prefix_theme = theme; if self.svc_logs.running { self.start_service_logs_task(); } }
                    ui.separator();
                    ui.label("Since(s):");
                    let mut since = self.svc_logs.since_seconds.unwrap_or(0);
                    if ui.add(egui::DragValue::new(&mut since).range(0..=86_400)).on_hover_text("Only show lines newer than N seconds; 0 disables").changed() {
                        self.svc_logs.since_seconds = if since <= 0 { None } else { Some(since as i64) };
                        if self.svc_logs.running { self.start_service_logs_task(); }
                    }
                    ui.separator();
                    ui.label("Tail:");
                    let mut tail = self.svc_logs.tail_lines.unwrap_or(0);
                    if ui.add(egui::DragValue::new(&mut tail).range(0..=10000)).on_hover_text("Tail last N lines; 0 disables").changed() {
                        self.svc_logs.tail_lines = if tail <= 0 { None } else { Some(tail as i64) };
                        if self.svc_logs.running { self.start_service_logs_task(); }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Grep:");
                    let resp = ui.add(egui::TextEdit::singleline(&mut self.svc_logs.grep).desired_width(200.0));
                    if resp.changed() {
                        let text = self.svc_logs.grep.trim().to_string();
                        if text.is_empty() { self.svc_logs.grep_cache = None; self.svc_logs.grep_error = None; }
                        else if let Ok(re) = regex::Regex::new(&text) { self.svc_logs.grep_cache = Some((text, re)); self.svc_logs.grep_error = None; }
                        else { self.svc_logs.grep_cache = None; self.svc_logs.grep_error = Some("invalid regex".into()); }
                    }
                    if let Some(err) = &self.svc_logs.grep_error { ui.colored_label(ui.visuals().warn_fg_color, err); }
                    if self.svc_logs.dropped > 0 { ui.separator(); ui.colored_label(ui.visuals().error_fg_color, format!("dropped: {}", self.svc_logs.dropped)); }
                    if ui.button("Clear").clicked() { self.svc_logs.ring.clear(); }
                    if !self.svc_logs.running { if ui.button("Start").clicked() { self.start_service_logs_task(); } }
                    else { if ui.button("Stop").clicked() { self.stop_service_logs_task(); } }
                });
        ui.add_space(4.0);

                // Build indices with grep
                let mut indices: Vec<usize> = if let Some((_t, re)) = &self.svc_logs.grep_cache {
                    self.svc_logs.ring.iter().enumerate().filter_map(|(i, p)| if re.is_match(&p.raw) { Some(i) } else { None }).collect()
                } else { (0..self.svc_logs.ring.len()).collect() };
                if !self.svc_logs.follow && self.svc_logs.order_by_ts_when_paused {
                    indices.sort_by(|&a, &b| {
                        let ta = self.svc_logs.ring.get(a).and_then(|p| p.timestamp.clone());
                        let tb = self.svc_logs.ring.get(b).and_then(|p| p.timestamp.clone());
                        match (ta, tb) {
                            (Some(x), Some(y)) => x.cmp(&y),
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                            (None, None) => a.cmp(&b),
                        }
                    });
                }
                if self.svc_logs.follow { let len = indices.len(); let take = self.svc_logs.visible_follow_limit.min(len); indices = indices.split_off(len - take); }

        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        let rows = indices.len();
        egui::ScrollArea::vertical()
            .id_salt("svc_logs_scroll")
            .stick_to_bottom(self.svc_logs.follow)
            .auto_shrink([false, false])
            .show_rows(ui, row_h, if self.svc_logs.follow { rows + self.svc_logs.follow_pad_rows } else { rows }, |ui, range| {
                for local_row in range.clone() {
                    if self.svc_logs.follow && local_row >= rows { ui.add_space(row_h); continue; }
                    if let Some(&ring_idx) = indices.get(local_row) {
                        if let Some(p) = self.svc_logs.ring.get(ring_idx) {
                            let widget = egui::Label::new(p.job.clone()).truncate();
                            ui.add(widget);
                        }
                    }
                }
            });
        if !self.svc_logs.follow && self.svc_logs.order_by_ts_when_paused {
            ui.colored_label(ui.visuals().weak_text_color(), "sorted by timestamp");
        }
    }

    pub(crate) fn select_row(&mut self, it: LiteObj) {
        info!(uid = ?it.uid, name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "details: selecting row");
        let uid = it.uid;
        self.details.selected = Some(uid);
        self.details.selected_at = Some(Instant::now());
        self.details.buffer.clear();
        // Reset Edit state for new selection; it will be initialized on next Detail update
        self.edit.buffer.clear();
        self.edit.original.clear();
        self.edit.dirty = false;
        self.edit.status.clear();
        self.details.secret_entries.clear();
        self.details.secret_revealed.clear();
        // Open/focus a dedicated tab for this resource
        self.open_details_tab_for(uid);
        // Clear pod-specific logs metadata on selection change
        self.logs.containers.clear();
        self.logs.container = None;
        // cancel previous detail task if any
        if let Some(stop) = self.details.stop.take() {
            info!("details: cancelling previous task");
            let _ = stop.send(());
        }
        // cancel describe task and reset output
        if let Some(task) = self.describe.task.take() { task.abort(); }
        if let Some(stop) = self.describe.stop.take() { let _ = stop.send(()); }
        self.describe.running = false;
        self.describe.text.clear();
        self.describe.error = None;
        self.describe.uid = None;
        // If details are cached and fresh, render immediately and skip fetch
        let now = Instant::now();
        if let Some((arc_text, maybe_cont, ts)) = self.details_cache.get(&uid).cloned() {
            if now.duration_since(ts).as_secs() <= self.details_ttl_secs {
                if let Some(tx) = self.watch.updates_tx.as_ref() {
                    let _ = tx.send(UiUpdate::Detail { uid, text: (*arc_text).clone(), containers: maybe_cont.clone(), produced_at: Instant::now() });
                }
                return;
            } else {
                // expired
                self.details_cache.remove(&uid);
            }
        }

        // need current kind (support both curated index selection and direct GVK selection)
        let Some(kind) = self.current_selected_kind().cloned() else { return; };
        // build reference
        let reference = ResourceRef { cluster: None, gvk: kind, namespace: it.namespace.clone(), name: it.name.clone() };
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.details.stop = Some(stop_tx);
        // spawn fetch task (debounced)
        let prefetch_ms: u64 = std::env::var("ORKA_DETAILS_PREFETCH_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        self.details.task = Some(tokio::spawn(async move {
            let t0 = Instant::now();
            info!(gvk = %gvk_label(&reference.gvk), name = %reference.name, ns = %reference.namespace.as_deref().unwrap_or("-"), "details: fetch start");
            let fetch = async {
                // small debounce to avoid thrashing on rapid selection changes
                tokio::time::sleep(std::time::Duration::from_millis(prefetch_ms)).await;
                match api.get_raw(reference).await {
                    Ok(bytes) => {
                        let maxb: usize = std::env::var("ORKA_DETAILS_MAX_BYTES").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1_500_000);
                        if bytes.len() > maxb {
                            let msg = format!("object too large to render ({} bytes > {}); adjust ORKA_DETAILS_MAX_BYTES to override", bytes.len(), maxb);
                            if let Some(tx) = tx_opt.as_ref() {
                                let _ = tx.send(UiUpdate::Detail { uid, text: msg.clone(), containers: None, produced_at: Instant::now() });
                            }
                            info!(size = bytes.len(), "details: skipped oversized object");
                            return;
                        }
                        // Parse JSON (if possible) both for YAML rendering and for extracting pod containers
                        let p0 = Instant::now();
                        let (text, containers, secret_entries): (String, Option<Vec<String>>, Option<Vec<crate::model::SecretEntry>>) = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                            Ok(v) => {
                                let parse_ms = p0.elapsed().as_millis() as f64;
                                histogram!("details_json_parse_ms", parse_ms);
                                // Extract pod containers if applicable
                                let e0 = Instant::now();
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
                                let extract_ms = e0.elapsed().as_millis() as f64;
                                histogram!("details_containers_extract_ms", extract_ms);
                                let mut uniq = std::collections::BTreeSet::new();
                                let dedup: Vec<String> = names.into_iter().filter(|n| uniq.insert(n.clone())).collect();
                                // Redact Secret values and collect entries
                                let mut redacted = v.clone();
                                let mut sec_entries: Option<Vec<crate::model::SecretEntry>> = None;
                                if v.get("kind").and_then(|k| k.as_str()) == Some("Secret") {
                                    if let Some(map) = v.get("data").and_then(|d| d.as_object()) {
                                        let mut items: Vec<crate::model::SecretEntry> = Vec::new();
                                        for (k, val) in map.iter() {
                                            if let Some(b64) = val.as_str() {
                                                let bytes = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()).unwrap_or_default();
                                                let decoded = String::from_utf8(bytes).ok();
                                                items.push(crate::model::SecretEntry { key: k.clone(), decoded, b64: b64.to_string() });
                                            }
                                        }
                                        if let Some(rm) = redacted.get_mut("data").and_then(|d| d.as_object_mut()) {
                                            let keys: Vec<String> = rm.keys().cloned().collect();
                                            for k in keys { rm.insert(k, serde_json::Value::String("[REDACTED]".into())); }
                                        }
                                        sec_entries = Some(items);
                                    }
                                }
                                let y0 = Instant::now();
                                let y = match serde_yaml::to_string(&redacted) { Ok(y) => y, Err(_) => String::from_utf8_lossy(&bytes).into_owned() };
                                let yaml_ms = y0.elapsed().as_millis() as f64;
                                histogram!("details_yaml_serialize_ms", yaml_ms);
                                info!(parse_ms, extract_ms, yaml_ms, "details: json→yaml timings");
                                (y, if dedup.is_empty() { None } else { Some(dedup) }, sec_entries)
                            }
                            Err(_) => (String::from_utf8_lossy(&bytes).into_owned(), None, None),
                        };
                        info!(size = bytes.len(), took_ms = %t0.elapsed().as_millis(), "details: fetch ok");
                        if let Some(tx) = tx_opt.as_ref() {
                            let _ = tx.send(UiUpdate::Detail { uid, text, containers: containers.clone(), produced_at: Instant::now() });
                            if let Some(entries) = secret_entries { let _ = tx.send(UiUpdate::SecretReady { uid, entries }); }
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
