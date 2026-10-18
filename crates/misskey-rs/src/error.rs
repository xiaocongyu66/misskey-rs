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
        if let Some(secs) = self.0.retry_after {
            if let Ok(v) = header::HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        response
    }
}
