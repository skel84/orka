#![forbid(unsafe_code)]

use eframe::egui;
use std::time::Instant;
use tracing::info;

use orka_api::ResourceRef;
use orka_core::LiteObj;

use crate::util::gvk_label;
use super::{OrkaGuiApp, UiUpdate};

impl OrkaGuiApp {
    pub(crate) fn ui_details(&mut self, ui: &mut egui::Ui) {
        ui.heading("Details");
        egui::ScrollArea::vertical()
            .id_salt("details_scroll")
            .show(ui, |ui| {
                if self.detail_buffer.is_empty() {
                    ui.label("Select a row to view details");
                } else {
                    let te = egui::TextEdit::multiline(&mut self.detail_buffer)
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
        self.selected = Some(it.uid);
        self.detail_buffer.clear();
        // cancel previous detail task if any
        if let Some(stop) = self.detail_stop.take() {
            info!("details: cancelling previous task");
            let _ = stop.send(());
        }
        // need current kind
        let Some(kind_idx) = self.selected_idx else { return; };
        let Some(kind) = self.kinds.get(kind_idx).cloned() else { return; };
        // build reference
        let reference = ResourceRef { cluster: None, gvk: kind, namespace: it.namespace.clone(), name: it.name.clone() };
        let api = self.api.clone();
        let tx_opt = self.updates_tx.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.detail_stop = Some(stop_tx);
        // spawn fetch task
        self.detail_task = Some(tokio::spawn(async move {
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
