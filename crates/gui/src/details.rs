#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;
use tracing::info;
use metrics::histogram;

use orka_api::ResourceRef;
use orka_core::LiteObj;

use crate::util::gvk_label;
use super::{OrkaGuiApp, UiUpdate};
use crate::model::DetailsPaneTab;

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

                // Fallback path when v2 is disabled or wrap is ON (multi-line breaks fixed-height rows)
                if !self.logs.v2 || self.logs.wrap {
                    let re_opt = self.logs.grep_cache.as_ref().map(|(_, r)| r);
                    let mut buf = String::new();
                    let mut shown = 0usize;
                    let max_lines: usize = 1000;
                    if self.logs.v2 {
                        // Use ring raw lines when v2 is enabled but wrapping is requested
                        for p in self.logs.ring.iter().rev() {
                            if let Some(r) = re_opt { if !r.is_match(&p.raw) { continue; } }
                            buf.push_str(&p.raw);
                            if !p.raw.ends_with('\n') { buf.push('\n'); }
                            shown += 1;
                            if shown >= max_lines { break; }
                        }
                    } else {
                        for line in self.logs.backlog.iter().rev() {
                            if let Some(r) = re_opt { if !r.is_match(line) { continue; } }
                            buf.push_str(line);
                            if !line.ends_with('\n') { buf.push('\n'); }
                            shown += 1;
                            if shown >= max_lines { break; }
                        }
                    }
                    let display = if shown == 0 { String::new() } else { buf.lines().rev().collect::<Vec<_>>().join("\n") };
                    let mut binding = display;
                    let te = egui::TextEdit::multiline(&mut binding)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(20)
                        .desired_width(f32::INFINITY)
                        .interactive(false);
                    ui.add(te);
                    return;
                }

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
                        #[cfg(feature = "dock")]
                        {
                            self.open_details_tab_for(uid);
                        }
                        #[cfg(not(feature = "dock"))]
                        {
                            self.layout.show_details = true;
                        }
                        ui.ctx().send_viewport_cmd_to(id, egui::ViewportCommand::Close);
                        self.detached.retain(|w| w.meta.id != id);
                    }
                } else {
                    if ui.small_button("Detach to Window").on_hover_text("Open this Details view in a separate OS window").clicked() {
                        let ctx = ui.ctx();
                        self.open_detached_for(ctx, uid);
                        #[cfg(feature = "dock")]
                        {
                            // Queue closing the corresponding dock tab, if any
                            self.dock_close_pending.push(uid);
                        }
                    }
                }
            } else {
                if ui.small_button("Detach to Window").clicked() {
                    self.toast("details: select a row first", crate::model::ToastKind::Info);
                }
            }
        });
        // Tab bar inside the Details pane (Edit | Logs | Svc Logs | Exec | Describe)
        ui.horizontal(|ui| {
            let tab = self.details.active_tab;
            let is_svc = self.selected_is_service();
            if ui.selectable_label(matches!(tab, DetailsPaneTab::Edit), "Edit").clicked() { self.details.active_tab = DetailsPaneTab::Edit; }
            if ui.selectable_label(matches!(tab, DetailsPaneTab::Logs), "Logs").clicked() { self.details.active_tab = DetailsPaneTab::Logs; }
            if is_svc { if ui.selectable_label(matches!(tab, DetailsPaneTab::SvcLogs), "Svc Logs").clicked() { self.details.active_tab = DetailsPaneTab::SvcLogs; } }
            if self.selected_is_pod() { if ui.selectable_label(matches!(tab, DetailsPaneTab::Exec), "Exec").clicked() { self.details.active_tab = DetailsPaneTab::Exec; } }
            if ui.selectable_label(matches!(tab, DetailsPaneTab::Describe), "Describe").clicked() { self.details.active_tab = DetailsPaneTab::Describe; }
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
                }
            });
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
        #[cfg(feature = "dock")]
        {
            // Open/focus a dedicated tab for this resource when docking is enabled
            self.open_details_tab_for(uid);
        }
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
                        // Parse JSON (if possible) both for YAML rendering and for extracting pod containers
                        let p0 = Instant::now();
                        let (text, containers): (String, Option<Vec<String>>) = match serde_json::from_slice::<serde_json::Value>(&bytes) {
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
                                let y0 = Instant::now();
                                let y = match serde_yaml::to_string(&v) { Ok(y) => y, Err(_) => String::from_utf8_lossy(&bytes).into_owned() };
                                let yaml_ms = y0.elapsed().as_millis() as f64;
                                histogram!("details_yaml_serialize_ms", yaml_ms);
                                info!(parse_ms, extract_ms, yaml_ms, "details: json→yaml timings");
                                (y, if dedup.is_empty() { None } else { Some(dedup) })
                            }
                            Err(_) => (String::from_utf8_lossy(&bytes).into_owned(), None),
                        };
                        info!(size = bytes.len(), took_ms = %t0.elapsed().as_millis(), "details: fetch ok");
                        if let Some(tx) = tx_opt.as_ref() {
                            let _ = tx.send(UiUpdate::Detail { uid, text, containers: containers.clone(), produced_at: Instant::now() });
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
