// mergemint-backend/src/main.rs
//
// Application entry-point: builds the Axum router with middleware and starts
// the HTTP server.
//
// ## Request body size limits and timeout middleware (#476)
//
// Two middleware layers are added to the router to protect the service from
// slow clients and excessively large payloads:
//
//   * `RequestBodyLimitLayer` — rejects bodies larger than `MAX_BODY_BYTES`.
//     Without this, a malicious client could stream an arbitrarily large body
//     and exhaust server memory before any handler logic runs.
//
//   * `TimeoutLayer` — cancels any request (including body reads and handler
//     execution) that takes longer than `REQUEST_TIMEOUT`.  This prevents slow
//     clients or downstream Horizon calls from holding connections indefinitely
//     and starving the thread pool.
//
// ## Request correlation IDs (#486)
//
// Every inbound request is stamped with a UUID v4 correlation ID by
// `SetRequestIdLayer`.  The ID is read from the `x-request-id` header when
// present (so callers can propagate their own trace ID), or generated fresh
// when absent.  `TraceLayer` then opens a tracing span for each request that
// includes the correlation ID, making it trivial to grep logs for a single
// user's flow even when requests are interleaved.
//
// ## CORS allow-list
//
// The CORS layer is built from an explicit, configured origin allow-list
// (`CORS_ALLOWED_ORIGINS`) rather than permitting `*`/`Any` — an
// unrestricted origin would let any website read authenticated responses
// from a browser holding a session with this API. An empty/unset value
// results in a layer that permits no cross-origin browser requests at all
// (same-origin and non-browser clients are unaffected), which is the safe
// default until the allow-list is configured for the environment.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    http::{header::CONTENT_TYPE, HeaderValue, Method},
    routing::{get, post},
    Router,
};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::Level;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod db;
mod routes;

use db::new_shared_db;
use routes::bounties::{list_bounties, list_bounties_by_assignee};
use routes::tx::{resolve_dispute, self_claim, AppState};

/// Maximum allowed request body size (1 MiB).
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Maximum wall-clock time allowed for a single request, including body reads
/// and handler execution.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The canonical header name used to carry the correlation ID across service
/// boundaries.  Clients may supply their own value; if absent a UUID v4 is
/// generated automatically by `SetRequestIdLayer`.
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Reward-token allowlist env var consumed by create-bounty flows.
const ALLOWLISTED_REWARD_TOKENS_ENV: &str = "ALLOWLISTED_REWARD_TOKENS";

/// Env var holding a comma-separated allow-list of origins permitted to make
/// cross-origin requests, e.g.
/// "https://app.mergemint.xyz,https://staging.mergemint.xyz".
const CORS_ALLOWED_ORIGINS_ENV: &str = "CORS_ALLOWED_ORIGINS";

#[tokio::main]
async fn main() {
    // ---------------------------------------------------------------------------
    // Initialise structured logging (#486)
    //
    // We use a layered subscriber so that:
    //  - RUST_LOG (or the compiled-in default "info") controls the verbosity.
    //  - Each log line is emitted as JSON-friendly structured output so the
    //    correlation ID that TraceLayer injects into the span is visible in
    //    every downstream log record without extra formatting work.
    // ---------------------------------------------------------------------------
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            // Default: info-level for our crate, warn for noisy deps.
            "mergemint_backend=info,tower_http=debug,axum::rejection=trace"
                .parse()
                .unwrap()
        }))
        .with(fmt::layer())
        .init();

    warn_if_reward_token_allowlist_empty();

    let shared_db = new_shared_db();
    let state = Arc::new(AppState { db: shared_db });

    let app = Router::new()
        .route("/tx/resolve-dispute", post(resolve_dispute))
        .route("/tx/self-claim", post(self_claim))
        .route("/bounties", get(list_bounties))
        .route(
            "/bounties/assignee/:address",
            get(list_bounties_by_assignee),
        )
        .with_state(state)
        // ── Correlation-ID middleware stack (#486) ──────────────────────────
        //
        // Layer order (innermost → outermost when receiving a request):
        //
        //  1. SetRequestIdLayer    — assigns x-request-id to every request that
        //                            does not already carry one.
        //  2. PropagateRequestIdLayer — copies the (possibly pre-existing)
        //                              x-request-id header into the response so
        //                              callers can correlate their own logs.
        //  3. TraceLayer           — opens a `tower_http::trace` span per
        //                            request; because it runs after the ID has
        //                            been set, the span automatically records
        //                            the correlation ID via the header extractor.
        //
        // NOTE: `.layer()` calls are applied bottom-up in Axum, so the layer
        // listed first in the source is closest to the handler.
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                let request_id = request
                    .headers()
                    .get(REQUEST_ID_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");
                tracing::span!(
                    Level::INFO,
                    "request",
                    request_id = %request_id,
                    method    = %request.method(),
                    uri       = %request.uri(),
                )
            }),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        // ── Body / timeout guards (#476) ────────────────────────────────────
        // Guard against slow-loris / oversized-body attacks (#476).
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        // Cancel requests that exceed the wall-clock budget (#476).
        .layer(TimeoutLayer::new(REQUEST_TIMEOUT))
        // Restrict cross-origin browser requests to the configured allow-list.
        .layer(build_cors_layer(
            &std::env::var(CORS_ALLOWED_ORIGINS_ENV).unwrap_or_default(),
        ));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("failed to bind TCP listener");

    tracing::info!(
        address = %listener.local_addr().unwrap(),
        "mergemint-backend listening"
    );

    axum::serve(listener, app).await.expect("server error");
}

fn warn_if_reward_token_allowlist_empty() {
    let allowlist = std::env::var(ALLOWLISTED_REWARD_TOKENS_ENV).unwrap_or_default();
    if allowlist.split(',').all(|token| token.trim().is_empty()) {
        tracing::warn!(
            "ALLOWLISTED_REWARD_TOKENS is empty — all create_bounty requests will be rejected"
        );
    }
}

/// Build the CORS layer from an explicit, comma-separated origin allow-list
/// string (see `CORS_ALLOWED_ORIGINS_ENV`). Kept separate from the env var
/// lookup so it's trivially testable with a fixed input.
fn build_cors_layer(allowed_origins: &str) -> CorsLayer {
    let origins: Vec<HeaderValue> = allowed_origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|origin| match origin.parse::<HeaderValue>() {
            Ok(value) => Some(value),
            Err(_) => {
                tracing::warn!(origin, "ignoring invalid CORS_ALLOWED_ORIGINS entry");
                None
            }
        })
        .collect();

    if origins.is_empty() {
        tracing::warn!(
            "CORS_ALLOWED_ORIGINS is empty — no cross-origin browser requests will be permitted"
        );
    }

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([CONTENT_TYPE])
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::Service;

    #[test]
    fn empty_allowlist_detection_handles_unset_empty_and_commas() {
        fn is_empty(value: &str) -> bool {
            value.split(',').all(|token| token.trim().is_empty())
        }

        assert!(is_empty(""));
        assert!(is_empty(" , , "));
        assert!(!is_empty("native"));
        assert!(!is_empty(" , native , "));
    }

    fn test_router(allowed_origins: &str) -> Router {
        Router::new()
            .route("/health", get(|| async { "ok" }))
            .layer(build_cors_layer(allowed_origins))
    }

    /// A request from an origin that isn't on the allow-list must not
    /// receive an `Access-Control-Allow-Origin` header — without it, a
    /// browser refuses to expose the response to that origin's script.
    #[tokio::test]
    async fn disallowed_origin_does_not_receive_cors_allow_header() {
        let mut app = test_router("https://app.mergemint.xyz");

        let request = Request::builder()
            .uri("/health")
            .header("origin", "https://evil.example.com")
            .body(Body::empty())
            .unwrap();

        let response = app.call(request).await.unwrap();

        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "a disallowed origin must not receive an Access-Control-Allow-Origin header"
        );
    }

    /// A request from an allow-listed origin must receive a matching
    /// `Access-Control-Allow-Origin` header.
    #[tokio::test]
    async fn allowed_origin_receives_cors_allow_header() {
        let mut app = test_router("https://app.mergemint.xyz");

        let request = Request::builder()
            .uri("/health")
            .header("origin", "https://app.mergemint.xyz")
            .body(Body::empty())
            .unwrap();

        let response = app.call(request).await.unwrap();

        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .expect("allowed origin must receive the header"),
            "https://app.mergemint.xyz"
        );
    }

    /// An empty allow-list (nothing configured) must permit no cross-origin
    /// requests at all — the safe default, never `*`.
    #[tokio::test]
    async fn empty_allow_list_grants_no_origin_access() {
        let mut app = test_router("");

        let request = Request::builder()
            .uri("/health")
            .header("origin", "https://app.mergemint.xyz")
            .body(Body::empty())
            .unwrap();

        let response = app.call(request).await.unwrap();

        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "an empty CORS_ALLOWED_ORIGINS must not grant any origin access"
        );
    }
}
