//! Wire-format API error: `{"error": {message, code, id, kind, info?}}`.
//!
//! Port of `packages/backend/src/server/api/error.ts`. Error codes/ids are
//! stable upstream UUIDs and MUST NOT be changed — clients assert on them.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiErrorKind {
    Client,
    Permission,
    Server,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub message: String,
    pub code: String,
    pub id: String,
    pub kind: ApiErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub body: ApiErrorBody,
    /// Seconds for the `Retry-After` header (used by RATE_LIMIT_EXCEEDED). Not serialized.
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(
        status: u16,
        kind: ApiErrorKind,
        code: &str,
        id: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            body: ApiErrorBody {
                message: message.into(),
                code: code.into(),
                id: id.into(),
                kind,
                info: None,
            },
            retry_after: None,
        }
    }

    pub fn with_info(mut self, info: serde_json::Value) -> Self {
        self.body.info = Some(info);
        self
    }

    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    /// Upstream `new ApiError()` with no argument.
    pub fn internal_error() -> Self {
        Self::new(
            500,
            ApiErrorKind::Server,
            "INTERNAL_ERROR",
            "5d37dbcb-891e-41ca-a3d6-e690c97775ac",
            "Internal error occurred. Please contact us if the error persists.",
        )
    }

    /// Unknown /api route, from `ApiServerService.ts` fastify.get('/*').
    pub fn unknown_endpoint() -> Self {
        Self::new(
            404,
            ApiErrorKind::Client,
            "UNKNOWN_API_ENDPOINT",
            "2ca3b769-540a-4f08-9dd5-b5a825b6d0f1",
            "Unknown API endpoint.",
        )
    }

    /// From `ApiCallService.ts` param validation failure.
    pub fn invalid_param(info: serde_json::Value) -> Self {
        Self::new(
            400,
            ApiErrorKind::Client,
            "INVALID_PARAM",
            "0b5f1631-7c1a-41a6-b399-cce335f34d85",
            "Invalid param.",
        )
        .with_info(info)
    }

    /// From `ApiCallService.ts` rate limiter.
    pub fn rate_limit_exceeded(info: serde_json::Value) -> Self {
        Self::new(
            429,
            ApiErrorKind::Client,
            "RATE_LIMIT_EXCEEDED",
            "d5826d14-3982-4d2e-8011-b9e9f02499ef",
            "Rate limit exceeded. Please try again later.",
        )
        .with_info(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_without_info() {
        let e = ApiError::unknown_endpoint();
        let v = serde_json::to_value(&e.body).unwrap();
        assert_eq!(v["code"], "UNKNOWN_API_ENDPOINT");
        assert_eq!(v["kind"], "client");
        assert!(v.get("info").is_none());
    }

    #[test]
    fn serializes_kind_permission() {
        let e = ApiError::new(403, ApiErrorKind::Permission, "X", "y", "z");
        let v = serde_json::to_value(&e.body).unwrap();
        assert_eq!(v["kind"], "permission");
    }
}
