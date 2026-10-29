use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

// One error shape for the whole API.
//
// Handlers used to return a bare StatusCode, which axum renders with an EMPTY
// body. A consumer receiving 404 from /dataset could not tell "this source is
// not in the cache yet" from "this source id does not exist", and a 403 said
// nothing about which permission was missing — the information existed at the
// point of failure and was thrown away on the way out.
//
// A service adds its own shorthands for the refusals its routes share (an id
// that is not configured, a resource a key may not see) in an extension trait
// of its own: Rust allows inherent methods only where the type is defined.

// The wire format. Separate from ApiError because the status code travels in
// the HTTP status, not in the body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Human-readable explanation. Stable enough to log, not to match on.
    pub error: String,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn admin_only() -> Self {
        Self::forbidden("this route requires an admin API key")
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    // The 401s come from the auth middleware rather than a handler, and were
    // the last bare StatusCodes left — an empty body on the one error every
    // new consumer hits first. Two messages, because the two states have
    // different fixes: no credential offered versus the wrong one.
    pub fn missing_api_key() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "missing API key: pass it in the X-API-Key header or as a Bearer token",
        )
    }

    pub fn invalid_api_key() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid API key")
    }

    // Only a cookie-authenticated write can produce this: a request carrying an
    // explicit key never needs a CSRF token. The wording says so, because the
    // first person to see it will be holding a curl command that works.
    pub fn csrf_required() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "missing or invalid CSRF token — a request authenticated by the \
             browser session cookie must carry the token from its login, in the \
             X-CSRF-Token header or a _csrf form field. Requests that present \
             an API key directly (X-API-Key or Authorization: Bearer) do not \
             need one",
        )
    }
}

// Implementing IntoResponse is what lets a handler return
// Result<T, ApiError>: axum calls this to turn the error into a response.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

// A refusal from auth-rs's middleware, in this crate's words. auth-rs
// says only which refusal it is; the wording, and the {"error": ...} body it
// travels in, are the web layer's.
impl From<auth_rs::http::Rejection> for ApiError {
    fn from(rejection: auth_rs::http::Rejection) -> Self {
        use auth_rs::http::Rejection;
        match rejection {
            Rejection::MissingApiKey => Self::missing_api_key(),
            Rejection::InvalidApiKey => Self::invalid_api_key(),
            Rejection::CsrfRequired => Self::csrf_required(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auth_rs::http::Rejection;

    async fn answer(error: ApiError) -> (StatusCode, String) {
        let response = error.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn every_error_is_a_status_and_a_json_body() {
        let cases = [
            (ApiError::not_found("no such thing"), StatusCode::NOT_FOUND),
            (ApiError::forbidden("not yours"), StatusCode::FORBIDDEN),
            (
                ApiError::internal("broke"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (ApiError::admin_only(), StatusCode::FORBIDDEN),
            (ApiError::bad_request("bad"), StatusCode::BAD_REQUEST),
        ];
        for (error, status) in cases {
            let (got, body) = answer(error).await;
            assert_eq!(got, status);
            assert!(body.starts_with("{\"error\":\""), "{body}");
        }
        let (_, body) = answer(ApiError::not_found("no such thing")).await;
        assert_eq!(body, r#"{"error":"no such thing"}"#);
    }

    // The middleware's refusals: the two 401s name different fixes, and the
    // CSRF one says a request with a key never needs a token.
    #[tokio::test]
    async fn a_refusal_from_the_middleware_names_its_fix() {
        let (status, body) = answer(Rejection::MissingApiKey.into()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("X-API-Key"), "{body}");

        let (status, body) = answer(Rejection::InvalidApiKey.into()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("invalid API key"), "{body}");

        let (status, body) = answer(Rejection::CsrfRequired.into()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("do not need one"), "{body}");
    }
}
