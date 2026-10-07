#![forbid(unsafe_code)]

pub mod backend;
pub mod comet;
pub mod config;
pub mod server;
pub mod transfer;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Deliberately contains no backend response bodies, URLs, passwords or tokens.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error(
        "backend transport failed; outcome may be unknown for a mutation, inspect state before retrying"
    )]
    Transport,
    #[error("backend rejected request (HTTP {0}); inspect state before retrying mutations")]
    Upstream(u16),
    #[error("backend response is incompatible or malformed")]
    Protocol,
    #[error("backend authentication failed or requires interactive verification")]
    Auth,
    #[error("unsupported capability: {0}")]
    Unsupported(&'static str),
    #[error("target switch refused while virtual media is connected; eject it first")]
    MediaConnected,
    #[error("transfer ticket is missing, expired, revoked or already consumed")]
    Ticket,
    #[error("too many active transfer tickets")]
    Busy,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Ticket => StatusCode::GONE,
            Self::Busy => StatusCode::TOO_MANY_REQUESTS,
            Self::MediaConnected => StatusCode::CONFLICT,
            Self::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
            _ => StatusCode::BAD_GATEWAY,
        };
        (
            status,
            axum::Json(serde_json::json!({"error":self.to_string()})),
        )
            .into_response()
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
