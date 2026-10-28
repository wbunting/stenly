//! stenly-share: the server behind private share links for Stenly meeting
//! notes. Notes are end-to-end encrypted by the desktop app; this server only
//! stores ciphertext (plus size and timestamps) and serves a viewer that
//! decrypts in the browser with the key from the URL fragment.
//!
//! Built on hearth (https://usehearth.dev). See `server/README.md` for self-hosting.

pub mod api;
pub mod app;
pub mod auth;
pub mod config;
pub mod dev_idp;
pub mod model;
pub mod oidc;
pub mod policy;
pub mod share;
pub mod store;
pub mod tables;

use std::sync::Arc;

use hearth::{asset::AssetBundle, router::Router};

/// App-context wrapper so the sweeper and handlers share one limiter.
pub struct ArcLimiter(pub Arc<api::RateLimiter>);

/// Builds the full router: routes, assets, database, config and policy.
/// `policy` is the env config overlaid with admin-saved settings
/// (`policy::load`).
pub fn build_router(cfg: config::Config, db: toasty::Db, assets: AssetBundle, limiter: Arc<api::RateLimiter>, policy: policy::Policy) -> Router {
    app::router(&cfg, assets)
        .app_context(db)
        .app_context(cfg)
        .app_context(ArcLimiter(limiter))
        .app_context(policy::PolicyCell::new(policy))
        .app_context(auth::PendingLogins::default())
        .app_context(auth::DiscoveryCache::default())
        .build()
}

/// The asset bundle: `STENLY_SHARE_ASSETS_DIR`, else `assets/` next to the binary.
pub fn load_assets() -> Result<AssetBundle, String> {
    match std::env::var("STENLY_SHARE_ASSETS_DIR") {
        Ok(dir) => AssetBundle::load_dir(&dir),
        Err(_) => AssetBundle::load(),
    }
    .map_err(|e| format!("loading the asset bundle (run `hearth asset bundle`, or set STENLY_SHARE_ASSETS_DIR): {e}"))
}
