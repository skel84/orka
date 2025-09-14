#![forbid(unsafe_code)]

use eframe::egui;
use egui_dock as dock;

use crate::{OrkaGuiApp, Tab};

impl OrkaGuiApp {
    /// Basic painter-based Atlas view (namespaces overview). Used by default
    /// and also as a fallback when optional graph renderer is enabled but not used.
    fn ui_atlas_global_basic(&mut self, ui: &mut egui::Ui) {
        use crate::watch::watch_hub_snapshot;
        const TOP_N: usize = 8;
        // Ensure namespaces are available even if no Kind is selected yet
        if self.namespaces.is_empty() {
            self.ensure_namespaces_watch();
        }
        // Controls
        ui.horizontal(|ui| {
            if ui.small_button("-").on_hover_text("Zoom out").clicked() {
                self.graph.atlas_zoom = (self.graph.atlas_zoom * 0.9).max(0.25);
            }
            if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                self.graph.atlas_zoom = (self.graph.atlas_zoom * 1.1).min(4.0);
            }
            if ui.small_button("Reset").on_hover_text("Reset view").clicked() {
                self.graph.atlas_zoom = 1.0;
                self.graph.atlas_pan = egui::vec2(0.0, 0.0);
            }
            ui.add(egui::Slider::new(&mut self.graph.atlas_zoom, 0.25..=4.0).text("Zoom"));
            ui.separator();
            ui.label("Click a Namespace to filter Results. Use sidebar to pick Kind.");
        });
        ui.add_space(6.0);

        // Drawing area (draggable for panning)
        let desired = egui::vec2(ui.available_width(), ui.available_height().max(280.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::drag());
        if response.dragged() {
            let d = response.drag_delta();
            self.graph.atlas_pan += d;
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

        // If no namespaces yet, show hint
        if self.namespaces.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "(Namespaces not loaded yet — select a Kind, or wait a moment)",
                egui::TextStyle::Body.resolve(ui.style()),
                ui.visuals().weak_text_color(),
            );
            return;
        }

        // Layout namespaces in a grid; expanded namespaces will render child kinds below them.
        let cols = 5usize;
        let spacing = egui::vec2(240.0, 160.0);
        let mut positions: Vec<(String, egui::Pos2)> = Vec::with_capacity(self.namespaces.len());
        for (i, ns) in self.namespaces.iter().cloned().enumerate() {
            let row = (i / cols) as f32;
            let col = (i % cols) as f32;
            let x = col * spacing.x;
            let y = row * spacing.y;
            positions.push((ns, egui::pos2(x, y)));
        }

        // Transform logical -> screen
        let center = rect.center() + self.graph.atlas_pan;
        let z = self.graph.atlas_zoom;
        let to_screen = |p: egui::Pos2| egui::pos2(center.x + p.x * z, center.y + p.y * z);

        // Draw namespaces and progressively disclose child nodes
        for (ns, lp) in positions {
            let p = to_screen(lp);
            let radius = 26.0 * z.clamp(0.5, 1.4);
            let selected = !self.selection.namespace.is_empty() && self.selection.namespace == ns;
            let fill = if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            };
            let stroke = if selected {
                egui::Stroke::new(3.0, ui.visuals().selection.stroke.color)
            } else {
                egui::Stroke::new(2.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
            };
            painter.circle_filled(p, radius, fill);
            painter.circle_stroke(p, radius, stroke);

            // Interaction hitbox
            let id = ui.make_persistent_id(("atlas_ns", &ns));
            let hit = egui::Rect::from_center_size(p, egui::vec2(160.0, 40.0));
            let resp = ui.interact(hit, id, egui::Sense::click());
            if resp.hovered() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                // Toggle selection of namespace; clicking selected clears filter
                if self.selection.namespace == ns { self.selection.namespace.clear(); } else { self.selection.namespace = ns.clone(); }
                // Toggle expansion state
                if self.graph.atlas_expanded_ns.contains(&ns) { self.graph.atlas_expanded_ns.remove(&ns); } else { self.graph.atlas_expanded_ns.insert(ns.clone()); }
            }
            // Label
            painter.text(
                p,
                egui::Align2::CENTER_CENTER,
                &ns,
                egui::TextStyle::Small.resolve(ui.style()),
                ui.visuals().strong_text_color(),
            );

            // If expanded: draw child kinds and counts
            if self.graph.atlas_expanded_ns.contains(&ns) {
                // kinds and mapping to gvk keys
                let kinds: [(&str, &str); 5] = [
                    ("Pods", "v1/Pod"),
                    ("Deployments", "apps/v1/Deployment"),
                    ("Services", "v1/Service"),
                    ("ConfigMaps", "v1/ConfigMap"),
                    ("Secrets", "v1/Secret"),
                ];
                for (i, (klabel, gvk)) in kinds.iter().enumerate() {
                    let dx = (-2.0 + i as f32) * 60.0; // spread under ns
                    let dy = 70.0;
                    let kp = to_screen(egui::pos2(lp.x + dx, lp.y + dy));
                    let kr = 18.0 * z.clamp(0.5, 1.3);
                    // count cache
                    let key = (ns.clone(), (*klabel).to_string());
                    let cnt = if let Some(c) = self.graph.atlas_counts.get(&key).copied() { c } else {
                        let items = watch_hub_snapshot(&format!("{}|{}", gvk, ns));
                        let c = items.len();
                        self.graph.atlas_counts.insert(key.clone(), c);
                        c
                    };
                    let color = match *klabel {
                        "Pods" => egui::Color32::from_rgb(86, 204, 149),
                        "Deployments" => egui::Color32::from_rgb(86, 156, 214),
                        "Services" => egui::Color32::from_rgb(255, 159, 64),
                        "ConfigMaps" => egui::Color32::from_rgb(255, 206, 86),
                        "Secrets" => egui::Color32::from_rgb(209, 99, 196),
                        _ => ui.visuals().hyperlink_color,
                    };
                    painter.circle_filled(kp, kr, color.gamma_multiply(0.9));
                    painter.circle_stroke(kp, kr, egui::Stroke::new(2.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
                    // label with count
                    let text = format!("{} ({})", klabel, cnt);
                    painter.text(kp + egui::vec2(0.0, 24.0), egui::Align2::CENTER_TOP, text, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().strong_text_color());
                    // interaction for kind node
                    let id = ui.make_persistent_id(("atlas_kind", &ns, *klabel));
                    let hit = egui::Rect::from_center_size(kp, egui::vec2(120.0, 40.0));
                    let resp = ui.interact(hit, id, egui::Sense::click());
                    if resp.hovered() { ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand); }
                    if resp.clicked() {
                        let kkey = (ns.clone(), (*klabel).to_string());
                        if self.graph.atlas_expanded_kinds.contains(&kkey) { self.graph.atlas_expanded_kinds.remove(&kkey); } else { self.graph.atlas_expanded_kinds.insert(kkey.clone()); }
                    }

                    // expanded items under kind (disabled in MVP)
                    let kkey = (ns.clone(), (*klabel).to_string());
                    if false && self.graph.atlas_expanded_kinds.contains(&kkey) {
                        let list = if let Some(v) = self.graph.atlas_items.get(&kkey) { v.clone() } else {
                            let items = watch_hub_snapshot(&format!("{}|{}", gvk, ns));
                            let mut names: Vec<String> = items.into_iter().map(|o| o.name).collect();
                            names.sort();
                            self.graph.atlas_items.insert(kkey.clone(), names.clone());
                            names
                        };
                        let mut shown = 0usize;
                        for (j, name) in list.into_iter().take(TOP_N).enumerate() {
                            let p2 = to_screen(egui::pos2(lp.x + dx, lp.y + dy + 34.0 + j as f32 * 20.0));
                            let rect = egui::Rect::from_center_size(p2, egui::vec2(160.0, 18.0));
                            painter.rect_filled(rect, 4.0, ui.visuals().widgets.inactive.bg_fill);
                            painter.rect_stroke(
                                rect,
                                4.0,
                                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                                egui::StrokeKind::Inside,
                            );
                            painter.text(rect.center(), egui::Align2::CENTER_CENTER, name, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().text_color());
                            shown += 1;
                        }
                        if let Some(c) = self.graph.atlas_counts.get(&kkey) {
                            if *c > shown {
                                let more = format!("+{} more", c - shown);
                                let p3 = to_screen(egui::pos2(lp.x + dx, lp.y + dy + 34.0 + shown as f32 * 20.0));
                                painter.text(p3, egui::Align2::CENTER_CENTER, more, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().weak_text_color());
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn ui_atlas_global(&mut self, ui: &mut egui::Ui) {
        self.ui_atlas_global_basic(ui);
    }

    /// Ensure the Atlas tab exists in the dock and focus it.
    pub(crate) fn open_atlas_tab(&mut self) {
        if !self.atlas_enabled { self.toast("atlas disabled (ORKA_ATLAS=0)", crate::model::ToastKind::Warn); return; }
        if let Some(ds) = self.dock.as_mut() {
            let tree = ds.main_surface_mut();
            if let Some((node, tab_index)) = tree.find_tab_from(|t| matches!(t, Tab::Atlas)) {
                tree.set_focused_node(node);
                tree.set_active_tab(node, tab_index);
            } else if let Some((node, _)) = tree.find_tab_from(|t| matches!(t, Tab::Results | Tab::Details | Tab::DetailsFor(_))) {
                tree.set_focused_node(node);
                tree.push_to_focused_leaf(Tab::Atlas);
            } else {
                tree.split_right(dock::NodeIndex::root(), 0.5, vec![Tab::Atlas]);
            }
        }
    }
}

// Eagerly start a namespaces watcher so the Atlas has content even with no Kind selection
impl OrkaGuiApp {
    fn ensure_namespaces_watch(&mut self) {
        use crate::model::UiUpdate;
        use crate::watch::{watch_hub_snapshot, watch_hub_subscribe};
        use orka_api::{ResourceKind, Selector};
        // Ensure there is an updates channel to deliver UiUpdate::Namespaces
        if self.watch.updates_tx.is_none() || self.watch.updates_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<UiUpdate>();
            self.watch.updates_tx = Some(tx);
            self.watch.updates_rx = Some(rx);
        }
        if self.watch.ns_task.is_some() { return; }
        let ns_tx = self.watch.updates_tx.as_ref().unwrap().clone();
        let ns_api = self.api.clone();
        let handle = tokio::spawn(async move {
            let key_ns = "v1/Namespace|".to_string();
            let ns_kind = ResourceKind { group: String::new(), version: "v1".into(), kind: "Namespace".into(), namespaced: false };
            let sel = Selector { gvk: ns_kind, namespace: None };
            match watch_hub_subscribe(ns_api.clone(), sel).await {
                Ok(mut rx) => {
                    let mut last_sent: usize = 0;
                    let mut send_list = || {
                        let mut list: Vec<String> = watch_hub_snapshot(&key_ns).into_iter().map(|o| o.name).collect();
                        list.sort();
                        list.dedup();
                        let len = list.len();
                        if len != last_sent { let _ = ns_tx.send(UiUpdate::Namespaces(list)); last_sent = len; }
                    };
                    send_list();
                    while let Ok(_) = rx.recv().await { send_list(); }
                }
                Err(_e) => { /* no-op; will retry when selection changes */ }
            }
        });
        self.watch.ns_task = Some(handle);
    }
}
