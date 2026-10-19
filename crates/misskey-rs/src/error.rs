//! Map `ApiError` onto axum responses: status + `{"error": {...}}` body,
//! matching `ApiCallService.send`.

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

pub use mk_types::api_error::ApiError;

/// Newtype wrapper — the orphan rule forbids implementing the foreign
/// `IntoResponse` trait directly for the foreign `ApiError` type.
pub struct ApiErrorResponse(pub ApiError);

pub fn api_err(e: ApiError) -> Response {
    ApiErrorResponse(e).into_response()
}

impl IntoResponse for ApiErrorResponse {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(json!({ "error": self.0.body }))).into_response();
        // arena-api: `ApiCallService.#sendApiError` WWW-Authenticate challenges
        // (L109-135). 401 → bare realm; the AuthenticationError path adds
        // invalid_token with its fixed description; every other 4xx client
        // error → invalid_request; PERMISSION_DENIED → insufficient_scope.
        // The rate-limit branch is upstream's own arm and adds none.
        let challenge: Option<String> = if status == StatusCode::UNAUTHORIZED {
            if self.0.body.code == "AUTHENTICATION_FAILED" {
                Some(
                    concat!(
                        "Bearer realm=\"Misskey\", error=\"invalid_token\", ",
                        "error_description=\"Authentication failed. ",
                        "Please ensure your token is correct.\""
                    )
                    .to_string(),
                )
            } else {
                Some("Bearer realm=\"Misskey\"".to_string())
            }
        } else if self.0.body.code != "RATE_LIMIT_EXCEEDED"
            && (StatusCode::BAD_REQUEST..StatusCode::INTERNAL_SERVER_ERROR).contains(&status)
        {
            // error_description carries no quotes (upstream does no escaping
            // either — the envelope messages are quote-free).
            match self.0.body.kind {
                mk_types::api_error::ApiErrorKind::Client => Some(format!(
                    "Bearer realm=\"Misskey\", error=\"invalid_request\", \
                     error_description=\"{}\"",
                    self.0.body.message
                )),
                mk_types::api_error::ApiErrorKind::Permission
                    if self.0.body.code == "PERMISSION_DENIED" =>
                {
                    Some(format!(
                        "Bearer realm=\"Misskey\", error=\"insufficient_scope\", \
                         error_description=\"{}\"",
                        self.0.body.message
                    ))
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(c) = challenge {
            if let Ok(v) = header::HeaderValue::from_str(&c) {
                response.headers_mut().insert(header::WWW_AUTHENTICATE, v);
            }
        }
        if let Some(secs) = self.0.retry_after {
            if let Ok(v) = header::HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        response
    }
}
