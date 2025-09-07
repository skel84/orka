use std::str::FromStr;

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use tracing::{error, info, warn};
use orka_store::spawn_ingest_with_projector;
use tokio::sync::mpsc;
use std::collections::HashMap;
use tokio::signal;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "orkactl", version, about = "Orka CLI (M0)")]
struct Cli {
    /// Output format
    #[arg(short = 'o', long = "output", value_enum, global = true, default_value_t = Output::Human)]
    output: Output,

    /// Kubernetes namespace (default: current context)
    #[arg(long = "ns", global = true)]
    namespace: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum Output { Human, Json }

#[derive(Subcommand, Debug)]
enum Commands {
    /// Discover served resources (incl. CRDs)
    Discover {
        /// Prefer selecting a CRD (for demos)
        #[arg(long = "prefer-crd", action = ArgAction::SetTrue)]
        prefer_crd: bool,
    },
    /// List objects for a given group/version/kind key
    Ls {
        /// GVK key, e.g. "v1/ConfigMap" or "cert-manager.io/v1/Certificate"
        gvk: String,
    },
    /// Watch objects for a GVK and print +/- events
    Watch {
        /// GVK key, e.g. "v1/ConfigMap" or "cert-manager.io/v1/Certificate"
        gvk: String,
    },
    /// Inspect schema details for a GVK (CRDs only)
    Schema {
        /// GVK key, e.g. "cert-manager.io/v1/Certificate"
        gvk: String,
    },
    /// Search current snapshot (simple RAM index)
    Search {
        /// GVK key to watch while indexing
        gvk: String,
        /// Query string (supports free text; typed filters TODO)
        query: String,
        /// Limit results
        #[arg(long = "limit", default_value_t = 20)]
        limit: usize,
        /// Explain filter stages and counts
        #[arg(long = "explain", action = ArgAction::SetTrue)]
        explain: bool,
    },
}

fn init_tracing() {
    let env = std::env::var("ORKA_LOG").unwrap_or_else(|_| "info".to_string());
    let filter = tracing_subscriber::EnvFilter::from_str(&env).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(true).init();
}

fn init_metrics() {
    if let Ok(addr) = std::env::var("ORKA_METRICS_ADDR") {
        if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
            let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
            match builder.with_http_listener(sock).install() {
                Ok(_) => tracing::info!(addr = %addr, "Prometheus metrics exporter listening"),
                Err(e) => tracing::warn!(error = %e, "failed to install metrics exporter"),
            }
        } else {
            tracing::warn!(addr = %addr, "invalid ORKA_METRICS_ADDR; expected host:port");
        }
    }
}

fn parse_gvk(key: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        [version, kind] => Some((String::new(), (*version).to_string(), (*kind).to_string())),
        [group, version, kind] => Some(((*group).to_string(), (*version).to_string(), (*kind).to_string())),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    init_metrics();
    let cli = Cli::parse();

    match cli.command {
        Commands::Discover { prefer_crd } => {
            info!(prefer_crd, "discover invoked");
            match orka_kubehub::discover(prefer_crd).await {
                Ok(resources) => match cli.output {
                    Output::Human => {
                        for r in resources {
                            let scope = if r.namespaced { "namespaced" } else { "cluster" };
                            let gv = if r.group.is_empty() { r.version.clone() } else { format!("{}/{}", r.group, r.version) };
                            println!("{} • {} • {}", gv, r.kind, scope);
                        }
                    }
                    Output::Json => println!("{}", serde_json::to_string_pretty(&resources)?),
                },
                Err(e) => {
                    error!(error = ?e, "discover failed");
                    eprintln!("discover error: {}", e);
                }
            }
        }
        Commands::Ls { gvk } => {
            let ns = cli.namespace.as_deref();
            info!(gvk = %gvk, ns = ?ns, "ls invoked");
            let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
            let projector = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(std::sync::Arc::new(schema.projector()) as std::sync::Arc<dyn orka_core::Projector + Send + Sync>),
                _ => None,
            };
            let (ingest_tx, backend) = spawn_ingest_with_projector(cap, projector);
            // Start watcher
            let watcher_handle = tokio::spawn({
                let gvk = gvk.clone();
                let ns = ns.map(|s| s.to_string());
                let tx = ingest_tx.clone();
                async move {
                    if let Err(e) = orka_kubehub::start_watcher(&gvk, ns.as_deref(), tx).await {
                        error!(error = ?e, "watcher failed");
                    }
                }
            });
            // Prime initial list for faster first snapshot
            let _ = orka_kubehub::prime_list(&gvk, ns, &ingest_tx).await;

            // Wait for first epoch (configurable)
            let wait_secs = std::env::var("ORKA_WAIT_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(8);
            let mut rx = backend.subscribe_epoch();
            let deadline = Instant::now() + Duration::from_secs(wait_secs);
            while *rx.borrow() == 0 {
                let now = Instant::now();
                if now >= deadline { break; }
                let rem = deadline.duration_since(now).min(Duration::from_secs(2));
                if tokio::time::timeout(rem, rx.changed()).await.is_err() { break; }
            }
            let snap = backend.current();

            match cli.output {
                Output::Human => {
                    println!("NAMESPACE   NAME                 AGE");
                    for item in snap.items.iter().filter(|o| ns.map(|n| o.namespace.as_deref() == Some(n)).unwrap_or(true)) {
                        let ns_col = item.namespace.clone().unwrap_or_else(|| "-".to_string());
                        let age = render_age(item.creation_ts);
                        println!("{:<11} {:<20} {}", ns_col, item.name, age);
                    }
                }
                Output::Json => {
                    // Filter by namespace if provided
                    let items: Vec<_> = snap.items
                        .iter()
                        .filter(|o| ns.map(|n| o.namespace.as_deref() == Some(n)).unwrap_or(true))
                        .cloned()
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&items)?);
                }
            }
            // Graceful shutdown: drop last sender and abort watcher to close ingest and flush
            drop(ingest_tx);
            watcher_handle.abort();
        }
        Commands::Watch { gvk } => {
            let ns = cli.namespace.as_deref();
            info!(gvk = %gvk, ns = ?ns, "watch invoked");
            let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
            let projector = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(std::sync::Arc::new(schema.projector()) as std::sync::Arc<dyn orka_core::Projector + Send + Sync>),
                _ => None,
            };
            let (ingest_tx, _backend) = spawn_ingest_with_projector(cap, projector);
            let (tap_tx, mut tap_rx) = mpsc::channel::<orka_core::Delta>(cap);

            // Start watcher writing into our tap
            let watcher_handle = tokio::spawn({
                let gvk = gvk.clone();
                let ns = ns.map(|s| s.to_string());
                let tap_tx = tap_tx.clone();
                async move {
                    if let Err(e) = orka_kubehub::start_watcher(&gvk, ns.as_deref(), tap_tx).await {
                        error!(error = ?e, "watcher failed");
                    }
                }
            });

            // Pump deltas into ingest and print lines directly
            let mut seen_rv: HashMap<orka_core::Uid, String> = HashMap::new();
            loop {
                tokio::select! {
                    maybe = tap_rx.recv() => {
                        match maybe {
                            Some(d) => {
                                // forward to ingest (best-effort)
                                let _ = ingest_tx.send(d.clone()).await;

                                // filter by ns if provided
                                if let Some(ns_filter) = ns {
                                    if let Some(mns) = d.raw.get("metadata").and_then(|m| m.get("namespace")).and_then(|v| v.as_str()) {
                                        if mns != ns_filter { continue; }
                                    } else {
                                        // cluster-scoped won't match a namespaced filter
                                        continue;
                                    }
                                }
                                let key = json_key(&d.raw);
                                let rv = d
                                    .raw
                                    .get("metadata")
                                    .and_then(|m| m.get("resourceVersion"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                match d.kind {
                                    orka_core::DeltaKind::Applied => {
                                        match seen_rv.get_mut(&d.uid) {
                                            None => {
                                                seen_rv.insert(d.uid, rv);
                                                println!("+ {}", key);
                                            }
                                            Some(prev_rv) => {
                                                if *prev_rv != rv {
                                                    *prev_rv = rv;
                                                    println!("+ {}", key);
                                                } // else duplicate; ignore
                                            }
                                        }
                                    }
                                    orka_core::DeltaKind::Deleted => {
                                        let _ = seen_rv.remove(&d.uid);
                                        println!("- {}", key);
                                    }
                                }
                            }
                            None => {
                                warn!("tap channel closed; exiting watch loop");
                                break;
                            }
                        }
                    }
                    _ = signal::ctrl_c() => {
                        info!("Ctrl-C received; shutting down watch loop");
                        break;
                    }
                }
            }

            // Graceful shutdown: close ingest and abort watcher to stop kube stream, allowing final snapshot flush
            drop(ingest_tx);
            watcher_handle.abort();
            warn!("watch loop ended (graceful shutdown)");
        }
        Commands::Schema { gvk } => {
            info!(gvk = %gvk, "schema invoked");
            match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => match cli.output {
                    Output::Human => {
                        println!("served: {}", schema.served_version);
                        if schema.printer_cols.is_empty() {
                            println!("printer-cols: (none)");
                        } else {
                            let cols: Vec<_> = schema.printer_cols.iter().map(|c| c.name.as_str()).collect();
                            println!("printer-cols: {}", cols.join(", "));
                        }
                        if schema.projected_paths.is_empty() {
                            println!("projected: (heuristic defaults)");
                        } else {
                            let proj: Vec<_> = schema.projected_paths.iter().map(|p| p.json_path.as_str()).collect();
                            println!("projected: {}", proj.join(", "));
                        }
                    }
                    Output::Json => {
                        println!("{}", serde_json::to_string_pretty(&schema)?);
                    }
                },
                Ok(None) => {
                    eprintln!("no CRD schema for builtin kind (or not found)");
                }
                Err(e) => {
                    eprintln!("schema error: {}", e);
                }
            }
        }
        Commands::Search { gvk, query, limit, explain } => {
            // Choose watcher namespace: CLI --ns overrides, else extract from query ns:token
            let ns_from_query = query.split_whitespace().find_map(|t| t.strip_prefix("ns:")).map(|s| s.to_string());
            let effective_ns = cli.namespace.clone().or(ns_from_query);
            let ns = effective_ns.as_deref();
            info!(gvk = %gvk, ns = ?ns, query = %query, limit, "search invoked");
            let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
            let projector = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(std::sync::Arc::new(schema.projector()) as std::sync::Arc<dyn orka_core::Projector + Send + Sync>),
                _ => None,
            };
            let (ingest_tx, backend) = spawn_ingest_with_projector(cap, projector);
            // Start watcher
            let watcher_handle = tokio::spawn({
                let gvk = gvk.clone();
                let ns = ns.map(|s| s.to_string());
                let tx = ingest_tx.clone();
                async move {
                    if let Err(e) = orka_kubehub::start_watcher(&gvk, ns.as_deref(), tx).await {
                        error!(error = ?e, "watcher failed");
                    }
                }
            });
            // Prime initial list so snapshot has data before waiting
            let _ = orka_kubehub::prime_list(&gvk, ns, &ingest_tx).await;

            // Wait for first epoch (configurable)
            let wait_secs = std::env::var("ORKA_WAIT_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(8);
            let mut rx = backend.subscribe_epoch();
            let deadline = Instant::now() + Duration::from_secs(wait_secs);
            while *rx.borrow() == 0 {
                let now = Instant::now();
                if now >= deadline { break; }
                let rem = deadline.duration_since(now).min(Duration::from_secs(2));
                if tokio::time::timeout(rem, rx.changed()).await.is_err() { break; }
            }
            let snap = backend.current();
            // Build index with field mapping (if schema known)
            let field_pairs: Option<Vec<(String, u32)>> = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(schema.projected_paths.iter().map(|p| (p.json_path.clone(), p.id)).collect()),
                _ => None,
            };
            let (group_str, _version_str, kind_str) = parse_gvk(&gvk).unwrap_or((String::new(), String::new(), String::new()));
            let index = match field_pairs {
                Some(pairs) => orka_search::Index::build_from_snapshot_with_meta(&snap, Some(&pairs), Some(&kind_str), Some(&group_str)),
                None => orka_search::Index::build_from_snapshot_with_meta(&snap, None, Some(&kind_str), Some(&group_str)),
            };
            let (hits, dbg) = index.search_with_debug(&query, limit);

            match cli.output {
                Output::Human => {
                    println!("KIND   NAMESPACE/NAME                SCORE");
                    for h in hits {
                        if let Some(obj) = snap.items.get(h.doc as usize) {
                            let ns_col = obj.namespace.clone().unwrap_or_else(|| "-".to_string());
                            println!("{:<6} {:<22} {:<20} {:.2}", kind_str, format!("{}/{}", ns_col, obj.name), "", h.score);
                        }
                    }
                }
                Output::Json => {
                    #[derive(serde::Serialize)]
                    struct Row<'a> { ns: &'a str, name: &'a str, score: f32 }
                    let rows: Vec<_> = hits
                        .into_iter()
                        .filter_map(|h| {
                            snap.items.get(h.doc as usize).map(|o| Row { ns: o.namespace.as_deref().unwrap_or("") , name: &o.name, score: h.score })
                        })
                        .collect();
                    if explain {
                        #[derive(serde::Serialize)]
                        struct Explain<'a, T> { hits: T, debug: &'a orka_search::SearchDebugInfo }
                        println!("{}", serde_json::to_string_pretty(&Explain { hits: rows, debug: &dbg })?);
                    } else {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    }
                }
            }
            if explain && matches!(cli.output, Output::Human) {
                eprintln!("debug: total={} after_ns={} after_label_keys={} after_labels={} after_anno_keys={} after_annos={} after_fields={}", dbg.total, dbg.after_ns, dbg.after_label_keys, dbg.after_labels, dbg.after_anno_keys, dbg.after_annos, dbg.after_fields);
            }

            // Shutdown
            drop(ingest_tx);
            watcher_handle.abort();
        }
    }

    Ok(())
}

fn render_age(creation_ts: i64) -> String {
    if creation_ts <= 0 { return "-".to_string(); }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let mut secs = (now - creation_ts).max(0) as u64;
    let days = secs / 86_400; secs %= 86_400;
    let hours = secs / 3600; secs %= 3600;
    let mins = secs / 60; secs %= 60;
    if days > 0 { format!("{}d{}h", days, hours) }
    else if hours > 0 { format!("{}h{}m", hours, mins) }
    else if mins > 0 { format!("{}m", mins) }
    else { format!("{}s", secs) }
}

fn json_key(v: &serde_json::Value) -> String {
    let meta = v.get("metadata");
    let name = meta.and_then(|m| m.get("name")).and_then(|v| v.as_str()).unwrap_or("");
    if let Some(ns) = meta.and_then(|m| m.get("namespace")).and_then(|v| v.as_str()) {
        format!("{}/{}", ns, name)
    } else {
        name.to_string()
    }
}
