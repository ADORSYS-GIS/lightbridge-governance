//! Mounts the registry router cratestack generates from
//! `schema/governance.cstack` (ADR-0009). CBOR is the payload for every route
//! here -- JSON is reserved for `/internal/v1/resolve` (#11), which is not
//! part of this router.
//!
//! Auth: the gateway (Authorino) authenticates the caller and forwards their
//! identity on `x-auth-id`, matching the ADR-0047 pattern already running in
//! production for other services here. This service trusts that header
//! because it is only reachable through the gateway, never directly.

use axum::Router;
use cratestack::{AuthProvider, CoolContext, CoolError, RequestContext, Value};
use cratestack_codec_cbor::CborCodec;
use governance_core::schema::cratestack_schema::{self, Cratestack};
use std::future::Future;

#[derive(Clone)]
pub struct GatewayAuthProvider;

impl AuthProvider for GatewayAuthProvider {
    type Error = CoolError;

    fn authenticate(
        &self,
        request: &RequestContext<'_>,
    ) -> impl Future<Output = Result<CoolContext, Self::Error>> + Send {
        let id = request
            .headers
            .get("x-auth-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        core::future::ready(Ok(match id {
            Some(id) => CoolContext::authenticated(vec![("id".to_owned(), Value::String(id))]),
            None => CoolContext::anonymous(),
        }))
    }
}

/// No `procedure` blocks in `governance.cstack` yet -- this registry is empty
/// until one lands (e.g. `issueIntegrationCredential`, per the `Integration`
/// model's doc comment).
#[derive(Clone)]
pub struct Procedures;

impl cratestack_schema::procedures::ProcedureRegistry for Procedures {}

pub fn build_router(db: Cratestack) -> Router {
    cratestack_schema::axum::router(db, Procedures, CborCodec, GatewayAuthProvider)
}
