#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;
use tracing::info;

use orka_api::ResourceRef;
use orka_core::LiteObj;

use crate::util::gvk_label;
use super::{OrkaGuiApp, UiUpdate};

impl OrkaGuiApp {
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
                // Explain section (collapsed by default)
                self.ui_explain(ui);
                if self.details.buffer.is_empty() {
                    ui.label("Select a row to view details");
                } else {
                    let te = egui::TextEdit::multiline(&mut self.details.buffer)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(24)
                        .desired_width(f32::INFINITY)
                        .interactive(false);
                    ui.add(te);
                }
            });
    }

    pub(crate) fn select_row(&mut self, it: LiteObj) {
        info!(uid = ?it.uid, name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "details: selecting row");
        self.details.selected = Some(it.uid);
        self.details.buffer.clear();
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
                        let text = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                            Ok(v) => match serde_yaml::to_string(&v) { Ok(y) => y, Err(_) => String::from_utf8_lossy(&bytes).into_owned() },
                            Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
                        };
                        info!(size = bytes.len(), took_ms = %t0.elapsed().as_millis(), "details: fetch ok");
                        if let Some(tx) = tx_opt.as_ref() { let _ = tx.send(UiUpdate::Detail(text)); }
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
