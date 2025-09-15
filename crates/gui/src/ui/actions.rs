#![forbid(unsafe_code)]

use eframe::egui;
use tracing::info;

use crate::OrkaGuiApp;

fn is_pod_selected(app: &OrkaGuiApp) -> bool {
    if let Some(k) = app.current_selected_kind() {
        k.group.is_empty() && k.version == "v1" && k.kind == "Pod"
    } else {
        false
    }
}

fn is_httpish(port: u16, name: Option<&str>) -> bool {
    let lname = name.unwrap_or("").to_ascii_lowercase();
    match port {
        80 | 8080 | 8000 | 3000 | 443 | 8443 => true,
        _ => lname.contains("http") || lname.contains("web"),
    }
}

pub(crate) fn ui_actions_bar(app: &mut OrkaGuiApp, ui: &mut egui::Ui) {
    let caps = app.ops.caps.clone();
    let has_caps = caps.is_some();
    let pod_sel = is_pod_selected(app);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Actions:").strong());

        // Logs (Pods only + RBAC)
        let logs_ok = pod_sel && caps.as_ref().map(|c| c.pods_log_get).unwrap_or(false);
        if logs_ok {
            if ui.button(if app.logs.running { "Logs (Stop)" } else { "Logs" }).clicked() {
                if app.logs.running {
                    info!("ui: logs stop click");
                    app.stop_logs_task();
                } else {
                    info!("ui: logs start click");
                    app.start_logs_task();
                }
            }
        } else {
            ui.add_enabled(false, egui::Button::new("Logs"))
                .on_hover_text(if !pod_sel { "Select a Pod" } else { "RBAC: pod/log get denied" });
        }

        ui.separator();

    // Exec (gated; not yet wired to a terminal UI)
    let exec_ok = pod_sel && caps.as_ref().map(|c| c.pods_exec_create).unwrap_or(false);
    if exec_ok {
        if ui.button("Exec…").clicked() { info!("ui: exec click"); app.details.active_tab = crate::model::DetailsPaneTab::Exec; app.start_exec_task(); }
        if ui.button("Open in Terminal").on_hover_text("Launch external terminal (Alacritty preferred) with kubectl exec").clicked() {
            info!("ui: exec external terminal click");
            app.open_external_exec();
        }
    } else {
        ui.add_enabled(false, egui::Button::new("Exec…")).on_hover_text("RBAC: pods/exec or Pod selection missing");
    }

        ui.separator();

        // Port-forward
        let pf_ok = pod_sel && caps.as_ref().map(|c| c.pods_portforward_create).unwrap_or(false);
        ui.label("PF:");
        let mut local = app.ops.pf_local;
        let mut remote = app.ops.pf_remote;
        ui.add_enabled(pf_ok, egui::DragValue::new(&mut local).range(1..=65535));
        ui.label("→");
        // Remote port picker: prefer discovered Pod ports, fallback to numeric input
        if !app.ops.pf_candidates.is_empty() {
            // Clamp selection and default to first
            let mut sel = app.ops.pf_selected_idx.unwrap_or(0).min(app.ops.pf_candidates.len().saturating_sub(1));
            // Always reflect the selected candidate in remote
            if let Some(p) = app.ops.pf_candidates.get(sel) { remote = p.port; }
            egui::ComboBox::from_id_salt("pf_remote_combo")
                .width(160.0)
                .selected_text({
                    let p = &app.ops.pf_candidates[sel];
                    if let Some(name) = &p.name { format!("{} ({})", p.port, name) } else { format!("{}", p.port) }
                })
                .show_ui(ui, |ui| {
                    for (i, p) in app.ops.pf_candidates.iter().enumerate() {
                        let mut label = p.port.to_string();
                        if let Some(name) = &p.name { label.push_str(&format!(" ({})", name)); }
                        ui.selectable_value(&mut sel, i, label);
                    }
                });
            if sel != app.ops.pf_selected_idx.unwrap_or(usize::MAX) {
                app.ops.pf_selected_idx = Some(sel);
                if let Some(p) = app.ops.pf_candidates.get(sel) { remote = p.port; if app.ops.pf_local == 0 { local = p.port; } }
            }
        } else {
            ui.add_enabled(pf_ok, egui::DragValue::new(&mut remote).range(1..=65535));
        }
        if local != app.ops.pf_local { app.ops.pf_local = local; }
        if remote != app.ops.pf_remote { app.ops.pf_remote = remote; }
        if !app.ops.pf_running {
            if pf_ok {
                if ui.button("Start").clicked() { info!("ui: pf start click"); app.start_port_forward_task(); }
            } else {
                ui.add_enabled(false, egui::Button::new("Start")).on_hover_text("RBAC: pods/portforward or Pod selection missing");
            }
        } else {
            let stop_pf = ui.button("Stop");
            if stop_pf.clicked() { info!("ui: pf stop click"); app.stop_port_forward(); }
            // When running and Ready was reported, show "Open in Browser" for HTTP(S) ports
            let show_open = app.ops.pf_ready_addr.is_some() && {
                let port = app.ops.pf_remote;
                let name = app
                    .ops
                    .pf_candidates
                    .iter()
                    .find(|c| c.port == port)
                    .and_then(|c| c.name.clone());
                is_httpish(port, name.as_deref())
            };
            if show_open {
                if ui.button("Open in Browser").clicked() {
                    app.open_pf_in_browser();
                }
            }
        }

        ui.separator();

        // Scale controls (simple absolute replicas input)
        let scale_ok = caps.as_ref().and_then(|c| c.scale.as_ref()).is_some();
        ui.label("Scale:");
        let mut r = app.ops.scale_replicas.max(0);
        ui.add_enabled(scale_ok, egui::DragValue::new(&mut r).range(0..=10000));
        if scale_ok {
            if ui.button("Apply").on_hover_text("Set replicas to this value").clicked() { info!(replicas = r, "ui: scale apply click"); app.ops.scale_replicas = r; app.start_scale_task(); }
        } else {
            ui.add_enabled(false, egui::Button::new("Apply")).on_hover_text(if has_caps { "Not scalable" } else { "Probing caps…" });
        }

        ui.separator();

        // Rollout restart (gate loosely on scale capability presence)
        let rr_ok = scale_ok; // approx until dedicated probe exists
        let rr_btn = ui.add_enabled(rr_ok, egui::Button::new("Rollout Restart"));
        if rr_ok && rr_btn.clicked() { app.start_rollout_restart_task(); }
        if !rr_ok { rr_btn.on_hover_text(if has_caps { "Not supported for this kind" } else { "Probing caps…" }); }

        ui.separator();

        // Delete Pod (with confirm)
        if pod_sel {
            if ui.button("Delete").on_hover_text("Delete selected Pod").clicked() {
                if let Some((ns, pod)) = app.current_pod_selection() { app.ops.confirm_delete = Some((ns, pod)); }
            }
        }

        // Node ops: Cordon/Uncordon/Drain
        if app.selected_is_node() {
            let caps_ref = caps.as_ref();
            let can_patch_nodes = caps_ref.map(|c| c.nodes_patch).unwrap_or(false);
            if can_patch_nodes {
                if ui.button("Cordon").clicked() { info!("ui: cordon click"); app.start_cordon_task(true); }
                if ui.button("Uncordon").clicked() { info!("ui: uncordon click"); app.start_cordon_task(false); }
            } else {
                ui.add_enabled(false, egui::Button::new("Cordon")).on_hover_text("RBAC: nodes patch denied");
                ui.add_enabled(false, egui::Button::new("Uncordon")).on_hover_text("RBAC: nodes patch denied");
            }

            let can_drain = can_patch_nodes; // optionally also gate on pods_eviction_create
            if can_drain {
                if ui.button("Drain…").clicked() { info!("ui: drain confirm open"); if let Some(node) = app.current_node_selection() { app.ops.confirm_drain = Some(node); } }
            } else {
                ui.add_enabled(false, egui::Button::new("Drain…")).on_hover_text("RBAC: nodes patch or evictions denied");
            }
        }

        ui.separator();

        // PFs popover toggle
        let pf_count = if app.ops.pf_info.is_some() { 1 } else { 0 };
        if ui.button(format!("PFs ({})", pf_count)).clicked() { info!("ui: pf panel open"); app.ops.pf_panel_open = true; }
    });

    // Confirm: Delete pod
    if let Some((ns, pod)) = app.ops.confirm_delete.clone() {
        let mut open = true;
        egui::Window::new("Confirm Delete Pod")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!("Delete pod {}/{}?", ns, pod));
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { app.ops.confirm_delete = None; }
                    if ui.button("Delete").clicked() { info!("ui: delete pod confirm"); app.start_delete_pod_task(); app.ops.confirm_delete = None; }
                });
            });
        if !open { app.ops.confirm_delete = None; }
    }

    // Confirm: Drain node
    if let Some(node) = app.ops.confirm_drain.clone() {
        let mut open = true;
        egui::Window::new("Confirm Drain Node")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -20.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!("Drain node {}?", node));
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { app.ops.confirm_drain = None; }
                    if ui.button("Drain").clicked() { info!("ui: drain confirm"); app.start_drain_task(); app.ops.confirm_drain = None; }
                });
            });
        if !open { app.ops.confirm_drain = None; }
    }

    // Scale prompt window
    if app.ops.scale_prompt_open {
        let mut open = app.ops.scale_prompt_open;
        let caps_ok = app.ops.caps.as_ref().and_then(|c| c.scale.as_ref()).is_some();
        egui::Window::new("Scale Workload")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -10.0))
            .show(ui.ctx(), |ui| {
                if !caps_ok { ui.label("Not scalable for this kind"); return; }
                ui.label("Replicas:");
                let mut r = app.ops.scale_replicas.max(0);
                if ui.add(egui::DragValue::new(&mut r).range(0..=10000)).changed() { app.ops.scale_replicas = r; }
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { app.ops.scale_prompt_open = false; }
                    if ui.button("Apply").clicked() { app.start_scale_task(); app.ops.scale_prompt_open = false; }
                });
            });
        app.ops.scale_prompt_open = open;
    }

    // Active PFs popover
    if app.ops.pf_panel_open {
        let mut open = app.ops.pf_panel_open;
        let mut stop_pf_clicked = false;
        egui::Window::new("Active Port-Forwards")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(380.0)
            .show(ui.ctx(), |ui| {
                if let Some(info) = app.ops.pf_info.clone() {
                    let line = format!("{}/{}  {} → {}", info.namespace, info.pod, info.local, info.remote);
                    ui.horizontal(|ui| {
                        ui.label(line);
                        if ui.button("Stop").clicked() { stop_pf_clicked = true; }
                    });
                } else {
                    ui.label("None");
                }
            });
        if stop_pf_clicked { app.stop_port_forward(); }
        app.ops.pf_panel_open = open;
    }
}
