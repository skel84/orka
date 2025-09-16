use std::str::FromStr;
use std::sync::Arc;

fn init_tracing() {
    let env = std::env::var("ORKA_LOG").unwrap_or_else(|_| "info".to_string());
    let filter = tracing_subscriber::EnvFilter::from_str(&env)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    init_tracing();
    let api = Arc::new(orka_api::InProcApi::new());
    if let Err(e) = orka_gui::run_native(api) {
        eprintln!("GUI error: {}", e);
        std::process::exit(1);
    }
}
