//! Production startup for `chuggernaut api`: env config → connected state →
//! serve. Mirrors the dispatcher's §12.4 pattern: fail fast on bad config.
//!
//! Env: `NATS_URL` (default `nats://localhost:4222`), `KEYS_DIR` (default
//! `/data/keys`; needs `jwt_private.pem`/`jwt_public.pem` from init, uses
//! `dispatcher.creds` when present), `BIND_ADDR` (default `0.0.0.0:8080`),
//! `UI_DIST` (optional; serve the built PWA from this directory),
//! `SESSION_TTL` (default `24h`, spec §7.1).

use crate::{ApiState, SharedState};
use auth::jwt::{JwtSigner, JwtVerifier};
use std::path::PathBuf;
use std::sync::Arc;
use store::NatsStore;

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

pub struct ApiConfig {
    pub nats_url: String,
    pub keys_dir: PathBuf,
    pub bind_addr: std::net::SocketAddr,
    pub ui_dist: Option<PathBuf>,
    pub session_ttl: chrono::Duration,
}

impl ApiConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind_addr = env_opt("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8080".into());
        let session_ttl = match env_opt("SESSION_TTL") {
            Some(s) => types::parse_duration(&s)
                .map_err(|e| anyhow::anyhow!("SESSION_TTL: {e}"))
                .map(chrono::Duration::from_std)??,
            None => chrono::Duration::hours(24),
        };
        Ok(Self {
            nats_url: env_opt("NATS_URL").unwrap_or_else(|| "nats://localhost:4222".into()),
            keys_dir: env_opt("KEYS_DIR")
                .unwrap_or_else(|| "/data/keys".into())
                .into(),
            bind_addr: bind_addr
                .parse()
                .map_err(|e| anyhow::anyhow!("BIND_ADDR {bind_addr:?}: {e}"))?,
            ui_dist: env_opt("UI_DIST").map(Into::into),
            session_ttl,
        })
    }
}

pub async fn run(config: ApiConfig) -> anyhow::Result<()> {
    let private = tokio::fs::read(config.keys_dir.join("jwt_private.pem"))
        .await
        .map_err(|e| anyhow::anyhow!("jwt_private.pem in {}: {e}", config.keys_dir.display()))?;
    let public = tokio::fs::read(config.keys_dir.join("jwt_public.pem"))
        .await
        .map_err(|e| anyhow::anyhow!("jwt_public.pem in {}: {e}", config.keys_dir.display()))?;

    let creds_path = config.keys_dir.join("dispatcher.creds");
    let store = match tokio::fs::read_to_string(&creds_path).await {
        Ok(creds) => NatsStore::connect_with_creds(&config.nats_url, &creds).await?,
        Err(_) => NatsStore::connect(&config.nats_url).await?,
    };

    let artifacts = match tokio::fs::read_to_string(config.keys_dir.join("age_artifacts.key")).await
    {
        Ok(identity) => {
            let crypto = store::ArtifactCrypto::with_identity(&identity)?;
            Some(store.artifacts(crypto).await?)
        }
        Err(_) => {
            tracing::warn!("no age_artifacts.key: transcripts and logs will not be served");
            None
        }
    };

    let state: SharedState = Arc::new(ApiState {
        store,
        signer: JwtSigner::from_pem(&private)?,
        verifier: JwtVerifier::from_pem(&public)?,
        session_ttl: config.session_ttl,
        artifacts,
    });
    if let Some(dist) = &config.ui_dist
        && !dist.join("index.html").exists()
    {
        tracing::warn!("UI_DIST {} has no index.html", dist.display());
    }
    crate::serve(state, config.bind_addr, config.ui_dist).await?;
    Ok(())
}
