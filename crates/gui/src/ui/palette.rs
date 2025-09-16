#![forbid(unsafe_code)]

use eframe::egui;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::model::PaletteItem;
use crate::util::{gvk_label, parse_gvk_key_to_kind};
use crate::watch::{watch_hub_prime, watch_hub_snapshot_all};
use crate::{OrkaGuiApp, UiUpdate};

pub(crate) fn ui_palette(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    if !app.palette.open {
        return;
    }
    let palette_width: f32 = app.palette.width_hint.clamp(520.0, 860.0);
    let list_row_h: f32 = 20.0;
    let list_max_rows: usize = 14; // visible rows target
    let list_max_h: f32 = list_row_h * (list_max_rows as f32) + 8.0;
    let min_h = list_max_h + 70.0; // input + padding
    let mut win_open = app.palette.open;
    egui::Window::new("cmd_k_palette")
        .open(&mut win_open)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -12.0))
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .default_size([palette_width, min_h])
        .min_size([palette_width, min_h])
        .show(ctx, |ui| {
            ui.set_min_width(palette_width);
            // Dense spacing
            ui.spacing_mut().item_spacing.y = 4.0;
            let te = egui::TextEdit::singleline(&mut app.palette.query)
                .hint_text("Global search: ns:prod k:Pod payments …")
                .desired_width(f32::INFINITY);
            let resp = ui.add(te);
            if app.palette.need_focus {
                resp.request_focus();
                app.palette.need_focus = false;
            }
            if resp.changed() {
                app.palette.changed_at = Some(std::time::Instant::now());
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                app.palette.open = false;
            }
            // Debounce build
            if let Some(t0) = app.palette.changed_at {
                if t0.elapsed().as_millis() as u64 >= app.palette.debounce_ms {
                    app.rebuild_palette_results();
                    app.palette.changed_at = None;
                }
            }
            ui.separator();
            // Mode toggle: Cached vs Global (prime watchers)
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mode:").weak());
                let cached = !app.palette.mode_global;
                if ui.selectable_label(cached, "Cached").clicked() {
                    app.palette.mode_global = false;
                }
                if ui.selectable_label(!cached, "Global").clicked() {
                    if !app.palette.mode_global {
                        app.palette.mode_global = true;
                        app.start_palette_global_prime();
                    }
                }
            });
            ui.add_space(4.0);
            // Keyboard selection
            let prev_sel = app.palette.sel;
            if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                let len = app.palette.results.len();
                if len > 0 {
                    let cur = app.palette.sel.unwrap_or(usize::MAX);
                    app.palette.sel = Some(if cur == usize::MAX {
                        0
                    } else {
                        (cur + 1) % len
                    });
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                let len = app.palette.results.len();
                if len > 0 {
                    let cur = app.palette.sel.unwrap_or(0);
                    app.palette.sel = Some(if cur == 0 { len - 1 } else { cur - 1 });
                }
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if esc {
                app.palette.open = false;
            }
            let scroll_to_selected = app.palette.sel != prev_sel;
            let mut chosen: Option<PaletteItem> = None;
            // Results list
            let font = egui::FontId::monospace(13.0);
            egui::ScrollArea::vertical()
                .max_height(list_max_h)
                .show(ui, |ui| {
                    ui.style_mut().spacing.interact_size.y = list_row_h;
                    for (idx, it) in app.palette.results.clone().into_iter().enumerate() {
                        let is_sel = app.palette.sel == Some(idx);
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(palette_width - 24.0, list_row_h),
                            egui::Sense::click(),
                        );
                        if is_sel {
                            ui.painter()
                                .rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
                        }
                        // primary with highlight
                        let mut job = egui::text::LayoutJob::default();
                        let normal = egui::text::TextFormat {
                            font_id: font.clone(),
                            color: ui.visuals().text_color(),
                            ..Default::default()
                        };
                        let hl = egui::text::TextFormat {
                            font_id: font.clone(),
                            color: ui.visuals().strong_text_color(),
                            ..Default::default()
                        };
                        let chars: Vec<char> = it.primary.chars().collect();
                        for (i, ch) in chars.iter().enumerate() {
                            let fmt = if it.hi_indices.binary_search(&i).is_ok() {
                                &hl
                            } else {
                                &normal
                            };
                            job.append(&ch.to_string(), 0.0, fmt.clone());
                        }
                        let galley = ui.fonts(|f| f.layout_job(job));
                        let text_pos =
                            egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y * 0.5);
                        ui.painter()
                            .galley(text_pos, galley, ui.visuals().text_color());
                        // right-aligned secondary with highlight
                        let mut job2 = egui::text::LayoutJob::default();
                        let chars2: Vec<char> = it.secondary.chars().collect();
                        for (i, ch) in chars2.iter().enumerate() {
                            let fmt = if it.hi_sec_indices.binary_search(&i).is_ok() {
                                &hl
                            } else {
                                &normal
                            };
                            job2.append(&ch.to_string(), 0.0, fmt.clone());
                        }
                        let galley2 = ui.fonts(|f| f.layout_job(job2));
                        let sec_pos = egui::pos2(
                            rect.right() - galley2.size().x - 8.0,
                            rect.center().y - galley2.size().y * 0.5,
                        );
                        ui.painter()
                            .galley(sec_pos, galley2, ui.visuals().text_color());
                        if is_sel && scroll_to_selected {
                            ui.scroll_to_rect(rect, None);
                        }
                        if resp.clicked() {
                            chosen = Some(it.clone());
                        }
                    }
                });
            if enter {
                if let Some(sel) = app
                    .palette
                    .sel
                    .and_then(|i| app.palette.results.get(i).cloned())
                {
                    chosen = Some(sel);
                }
            }
            if let Some(item) = chosen.take() {
                app.open_palette_item(item);
                app.palette.open = false;
            }
        });
    app.palette.open = win_open;
}

pub(crate) fn handle_palette_shortcut(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    if ctx.input(|i| (i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::K)) {
        app.palette.open = true;
        app.palette.sel = None;
        app.palette.need_focus = true;
    }
}

impl OrkaGuiApp {
    pub(crate) fn rebuild_palette_results(&mut self) {
        self.palette.results.clear();
        let raw = self.palette.query.trim();
        if raw.is_empty() {
            return;
        }
        // Parse simple typed filters: ns:, k:, g:. Rest is fuzzy free text
        let mut ns_filter: Option<String> = None;
        let mut k_filter: Option<String> = None;
        let mut g_filter: Option<String> = None;
        let mut free_tokens: Vec<String> = Vec::new();
        for tok in raw.split_whitespace() {
            if let Some(v) = tok.strip_prefix("ns:") {
                ns_filter = Some(v.to_string());
            } else if let Some(v) = tok.strip_prefix("k:") {
                k_filter = Some(v.to_string());
            } else if let Some(v) = tok.strip_prefix("g:") {
                g_filter = Some(v.to_string());
            } else {
                free_tokens.push(tok.to_string());
            }
        }
        let free_q = free_tokens.join(" ").to_lowercase();
        let matcher = SkimMatcherV2::default();
        let all = watch_hub_snapshot_all();
        let mut scored: Vec<PaletteItem> = Vec::new();
        for (gvk_key, it) in all.into_iter() {
            let (group, _version, kind) = {
                let parts: Vec<&str> = gvk_key.split('/').collect();
                match parts.as_slice() {
                    [v, k] => ("".to_string(), (*v).to_string(), (*k).to_string()),
                    [g, v, k] => ((*g).to_string(), (*v).to_string(), (*k).to_string()),
                    _ => (String::new(), String::new(), String::new()),
                }
            };
            if let Some(kf) = k_filter.as_deref() {
                if !kind.eq_ignore_ascii_case(kf) {
                    continue;
                }
            }
            if let Some(gf) = g_filter.as_deref() {
                if !group.eq_ignore_ascii_case(gf) {
                    continue;
                }
            }
            if let Some(nsq) = ns_filter.as_deref() {
                let ns_it = it.namespace.as_deref().unwrap_or("");
                if ns_it != nsq {
                    continue;
                }
            }
            let hay = self
                .results
                .filter_cache
                .get(&it.uid)
                .cloned()
                .unwrap_or_else(|| self.build_filter_haystack(&it));
            let primary = format!("{}/{}", it.namespace.as_deref().unwrap_or("-"), it.name);
            // Score over haystack (name/ns/labels/projected) for recall
            let score = if free_q.is_empty() {
                0f32
            } else {
                matcher.fuzzy_match(&hay, &free_q).unwrap_or(-10) as f32
            };
            if free_q.is_empty() || score >= 0f32 {
                // Highlight both primary and secondary (gvk/score)
                let hi = if free_q.is_empty() {
                    Vec::new()
                } else {
                    matcher
                        .fuzzy_indices(&primary, &free_q)
                        .map(|(_, idx)| idx)
                        .unwrap_or_default()
                };
                let secondary = format!("{}   ({:.2})", gvk_key, score);
                let hi_sec = if free_q.is_empty() {
                    Vec::new()
                } else {
                    matcher
                        .fuzzy_indices(&secondary, &free_q)
                        .map(|(_, idx)| idx)
                        .unwrap_or_default()
                };
                scored.push(PaletteItem {
                    gvk_key: gvk_key.clone(),
                    obj: it.clone(),
                    score,
                    primary,
                    hi_indices: hi,
                    secondary,
                    hi_sec_indices: hi_sec,
                });
            }
        }
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.obj.name.cmp(&b.obj.name))
        });
        self.palette.results = scored.into_iter().take(50).collect();
        self.palette.sel = if self.palette.results.is_empty() {
            None
        } else {
            Some(0)
        };
        // Width hint based on visible text lengths
        let mut max_p = 0usize;
        let mut max_s = 0usize;
        for it in self.palette.results.iter().take(20) {
            max_p = max_p.max(it.primary.len());
            max_s = max_s.max(it.secondary.len());
        }
        let est = 60.0 + (max_p as f32) * 7.5 + (max_s as f32) * 6.5;
        self.palette.width_hint = est.clamp(520.0, 860.0);
    }

    pub(crate) fn open_palette_item(&mut self, item: PaletteItem) {
        // Resolve ResourceKind (with proper namespaced) from discovery list; fallback to parser
        let rk = self
            .discovery
            .kinds
            .iter()
            .find(|k| gvk_label(k) == item.gvk_key)
            .cloned()
            .unwrap_or_else(|| parse_gvk_key_to_kind(&item.gvk_key));
        self.selection.selected_kind = Some(rk);
        // Update namespace selector to the item's namespace if present
        self.selection.namespace = item.obj.namespace.clone().unwrap_or_default();
        // Open details directly
        self.select_row(item.obj);
    }

    pub(crate) fn start_palette_global_prime(&mut self) {
        if self.palette.prime_task.is_some() {
            return;
        }
        let api = self.api.clone();
        let kinds_opt = if !self.discovery.kinds.is_empty() {
            Some(self.discovery.kinds.clone())
        } else {
            None
        };
        let tx_opt = self.watch.updates_tx.clone();
        self.palette.prime_task = Some(tokio::spawn(async move {
            // Discover kinds if not provided
            let kinds = if let Some(k) = kinds_opt {
                k
            } else {
                match api.discover().await {
                    Ok(v) => v,
                    Err(_) => Vec::new(),
                }
            };
            for k in kinds.into_iter() {
                // Prime fast first page to avoid heavy calls
                let gvk_key = if k.group.is_empty() {
                    format!("{}/{}", k.version, k.kind)
                } else {
                    format!("{}/{}/{}", k.group, k.version, k.kind)
                };
                match orka_kubehub::list_lite_first_page(&gvk_key, None).await {
                    Ok(items) => {
                        let key = format!("{}|", gvk_key);
                        watch_hub_prime(&key, items);
                        if let Some(tx) = &tx_opt {
                            let _ = tx.send(UiUpdate::Error(String::new()));
                        } // nudge repaint
                    }
                    Err(_e) => {}
                }
            }
        }));
    }
}
