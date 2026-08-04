//! The OIDC issuer's public documents, served unauthenticated (design #313 A4,
//! spec §6.7).
//!
//! - **Accepts:** credential-free `GET`s of `/.well-known/openid-configuration`
//!   and `/.well-known/jwks.json`.
//! - **Emits:** the discovery document and the RFC 7517 JWK set, built once at
//!   startup from the configured issuer and the mounted `oidc_public.pem`.
//! - **Guarantees:** [`public_routes`] is the whole of the api's authentication
//!   exemption for these two paths — its handlers take no `Auth` extractor by
//!   design, and no project data, secret or private key is reachable through
//!   them. Serving them is not exposing them: the api binds loopback
//!   (`deploy/prod/run-api.sh`) and nothing here changes that.
//! - **Spec:** §6.7.

use crate::SharedState;
use crate::routes::ApiError;
use auth::oidc::{DiscoveryDocument, JwkSet};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// The issuer documents this api publishes, built once at startup so a bad key
/// or issuer fails the process rather than a request.
pub struct IssuerDocuments {
    discovery: DiscoveryDocument,
    jwks: JwkSet,
}

impl IssuerDocuments {
    /// Errors when `public_pem` is not an RSA public key; `issuer` must already
    /// have come from [`auth::oidc::issuer_from_env`].
    pub fn new(issuer: &str, public_pem: &str) -> Result<Self, auth::AuthError> {
        let jwks = auth::oidc::jwk_set_from_public_pem(public_pem)?;
        assert_eq!(jwks.keys.len(), 1, "the api publishes one issuer key");
        Ok(Self {
            discovery: auth::oidc::discovery_document(issuer),
            jwks,
        })
    }

    /// The `kid` a JWKS consumer will find in the published set.
    pub fn kid(&self) -> &str {
        &self.jwks.keys[0].kid
    }
}

/// The api's two unauthenticated routes, built apart from the authenticated
/// surface so the exemption is a decision and not an omission.
pub fn public_routes() -> axum::Router<SharedState> {
    axum::Router::new()
        .route(auth::oidc::DISCOVERY_PATH, get(discovery))
        .route(auth::oidc::JWKS_PATH, get(jwks))
}

async fn discovery(State(state): State<SharedState>) -> Result<Response, ApiError> {
    let documents = discovery_documents(&state)?;
    Ok(Json(&documents.discovery).into_response())
}

async fn jwks(State(state): State<SharedState>) -> Result<Response, ApiError> {
    let documents = discovery_documents(&state)?;
    Ok(Json(&documents.jwks).into_response())
}

/// The mounted issuer, or a 404 — a platform whose `oidc_public.pem` predates
/// §12.1's issuer keypair publishes nothing rather than an empty key set.
fn discovery_documents(state: &SharedState) -> Result<&IssuerDocuments, ApiError> {
    state.oidc.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "no oidc issuer key is mounted on this platform",
        )
    })
}
