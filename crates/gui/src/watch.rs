#![forbid(unsafe_code)]

use once_cell::sync::OnceCell;
use tokio::sync::broadcast;
use std::sync::Mutex;

use orka_api::{LiteEvent, OrkaApi, Selector};
use orka_core::{LiteObj, Uid};

use crate::util::gvk_label;

struct WatchHub {
    map: Mutex<std::collections::HashMap<String, broadcast::Sender<LiteEvent>>>,
    cache: Mutex<std::collections::HashMap<String, std::collections::HashMap<Uid, LiteObj>>>,
}

static WATCH_HUB: OnceCell<WatchHub> = OnceCell::new();

fn watch_hub() -> &'static WatchHub {
    WATCH_HUB.get_or_init(|| WatchHub {
        map: Mutex::new(std::collections::HashMap::new()),
        cache: Mutex::new(std::collections::HashMap::new()),
    })
}

pub(crate) async fn watch_hub_subscribe(api: std::sync::Arc<dyn OrkaApi>, sel: Selector) -> Result<broadcast::Receiver<LiteEvent>, String> {
    let key = format!("{}|{}", gvk_label(&sel.gvk), sel.namespace.as_deref().unwrap_or(""));
    // Fast path: existing
    if let Some(tx) = watch_hub().map.lock().unwrap().get(&key).cloned() {
        return Ok(tx.subscribe());
    }
    // Create sender and spawn underlying watcher task
    let (tx, rx) = broadcast::channel::<LiteEvent>(2048);
    watch_hub().map.lock().unwrap().insert(key.clone(), tx.clone());
    tokio::spawn(async move {
        match api.watch_lite(sel).await {
            Ok(mut sh) => {
                loop {
                    match sh.rx.recv().await {
                        Some(LiteEvent::Applied(lo)) => {
                            // Update cache then broadcast
                            let mut cache = watch_hub().cache.lock().unwrap();
                            let entry = cache.entry(key.clone()).or_insert_with(|| std::collections::HashMap::new());
                            entry.insert(lo.uid, lo.clone());
                            let _ = tx.send(LiteEvent::Applied(lo));
                        }
                        Some(LiteEvent::Deleted(lo)) => {
                            let mut cache = watch_hub().cache.lock().unwrap();
                            if let Some(map) = cache.get_mut(&key) { map.remove(&lo.uid); }
                            let _ = tx.send(LiteEvent::Deleted(lo));
                        }
                        None => break,
                    }
                }
            }
            Err(_e) => { /* keep map entry; clients may retry */ }
        }
    });
    Ok(rx)
}

pub(crate) fn watch_hub_snapshot(gvk_ns_key: &str) -> Vec<LiteObj> {
    let cache = watch_hub().cache.lock().unwrap();
    if let Some(map) = cache.get(gvk_ns_key) {
        map.values().cloned().collect()
    } else {
        Vec::new()
    }
}

pub(crate) fn watch_hub_prime(gvk_ns_key: &str, items: Vec<LiteObj>) {
    let mut cache = watch_hub().cache.lock().unwrap();
    let entry = cache.entry(gvk_ns_key.to_string()).or_insert_with(|| std::collections::HashMap::new());
    for it in items {
        entry.insert(it.uid, it);
    }
}

