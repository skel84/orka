#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

impl OrkaGuiApp {
    pub(crate) fn start_stats_task(&mut self) {
        // If already loading, don't start another
        if self.stats.loading {
            return;
        }
        self.stats.loading = true;
        self.stats.last_error = None;
        // Ensure updates channel exists
        let tx = if let Some(tx0) = &self.watch.updates_tx {
            tx0.clone()
        } else {
            let (tx0, rx0) = std::sync::mpsc::channel::<UiUpdate>();
            self.watch.updates_tx = Some(tx0.clone());
            self.watch.updates_rx = Some(rx0);
            tx0
        };
        let api = self.api.clone();
        // Cancel any previous task (not strictly needed for a one-shot)
        self.stats.task = Some(tokio::spawn(async move {
            match api.stats().await {
                Ok(s) => {
                    let metrics_addr = s.metrics_addr.clone();
                    let _ = tx.send(UiUpdate::StatsReady(s));
                    // Best-effort scrape of Prometheus metrics for index gauges
                    if let Some(addr) = metrics_addr {
                        if let Ok((ib, id)) = scrape_index_metrics(&addr).await {
                            let _ = tx.send(UiUpdate::MetricsReady {
                                index_bytes: ib,
                                index_docs: id,
                            });
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(UiUpdate::Error(format!("stats: {}", e)));
                }
            }
        }));
    }
}

async fn scrape_index_metrics(addr: &str) -> Result<(Option<u64>, Option<u64>), ()> {
    // Connect to host:port and GET /metrics; very small, blocking-safe over tokio
    let mut stream = TcpStream::connect(addr).await.map_err(|_| ())?;
    let req = format!(
        "GET /metrics HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        addr
    );
    stream.write_all(req.as_bytes()).await.map_err(|_| ())?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.map_err(|_| ())?;
    let text = String::from_utf8_lossy(&buf);
    // Split headers/body
    let body = match text.split("\r\n\r\n").nth(1) {
        Some(b) => b,
        None => &text,
    };
    let mut index_bytes: Option<u64> = None;
    let mut index_docs: Option<u64> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("index_bytes") {
            if let Some(val) = line.split_whitespace().last() {
                if let Ok(v) = val.parse::<f64>() {
                    index_bytes = Some(v as u64);
                }
            }
        } else if line.starts_with("index_docs") {
            if let Some(val) = line.split_whitespace().last() {
                if let Ok(v) = val.parse::<f64>() {
                    index_docs = Some(v as u64);
                }
            }
        }
    }
    Ok((index_bytes, index_docs))
}
