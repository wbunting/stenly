//! `stenly-share` binary: reads `STENLY_SHARE_*` and serves on `HOST`:`PORT`.

use std::sync::Arc;

use stenly_share::{api, build_router, config, load_assets, policy, store};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("stenly-share: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => {}
        Some("--version" | "version") => {
            println!("stenly-share {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => return Err(format!("unknown command {other:?} (usage: stenly-share [serve|version])")),
    }

    let cfg = config::Config::from_env()?;
    let mut db = store::open(&cfg).await.map_err(|e| format!("opening database in {}: {e}", cfg.data_dir.display()))?;
    let policy = policy::load(&mut db, &cfg).await.map_err(|e| format!("loading settings: {e}"))?;
    let assets = load_assets()?;

    let limiter = Arc::new(api::RateLimiter::default());
    store::spawn_sweeper(db.clone(), cfg.clone(), limiter.clone());
    if cfg.base_url.is_none() {
        eprintln!("stenly-share: STENLY_SHARE_BASE_URL is not set; share URLs use the request's Host header");
    }
    eprintln!(
        "stenly-share {} · data {} · max {} bytes · upload token {} · auth {}",
        env!("CARGO_PKG_VERSION"),
        cfg.data_dir.display(),
        policy.max_bytes,
        if cfg.upload_token.is_some() { "required" } else { "not required" },
        cfg.oidc.as_ref().map_or_else(|| "none".to_owned(), |o| format!("oidc ({})", o.issuer)),
    );
    hearth::start(build_router(cfg, db, assets, limiter, policy)).await.map_err(|e| e.to_string())
}
