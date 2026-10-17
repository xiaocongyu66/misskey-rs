//! Map `ApiError` onto axum responses: status + `{"error": {...}}` body,
//! matching `ApiCallService.send`.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub use mk_types::api_error::ApiError;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(json!({ "error": self.body }))).into_response();
        if let Some(secs) = self.retry_after {
            if let Ok(v) = header::HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        response
    }
}
