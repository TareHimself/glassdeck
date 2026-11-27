//! How a failure becomes an API answer. A failure the request caused is marked with its [`ErrorCode`]
//! where it happens; anything unmarked is an internal error.

use std::fmt;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use glassdeck_api::{ApiError, ErrorCode};

use crate::link::LinkDown;

/// A failure marked with what it means for the request.
#[derive(Debug)]
pub struct Refused {
    pub code: ErrorCode,
    pub message: String,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

fn refused(code: ErrorCode, message: impl fmt::Display) -> anyhow::Error {
    Refused { code, message: message.to_string() }.into()
}

pub fn bad_request(message: impl fmt::Display) -> anyhow::Error {
    refused(ErrorCode::BadRequest, message)
}

pub fn conflict(message: impl fmt::Display) -> anyhow::Error {
    refused(ErrorCode::Conflict, message)
}

pub fn not_found(message: impl fmt::Display) -> anyhow::Error {
    refused(ErrorCode::NotFound, message)
}

/// What a handler fails with: any error, answered with the code it was marked with.
pub struct Failure(anyhow::Error);

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

impl From<JsonRejection> for Failure {
    fn from(rejection: JsonRejection) -> Self {
        Self(bad_request(rejection.body_text()))
    }
}

impl From<PathRejection> for Failure {
    fn from(rejection: PathRejection) -> Self {
        Self(bad_request(rejection.body_text()))
    }
}

pub fn code_of(error: &anyhow::Error) -> ErrorCode {
    if let Some(refused) = error.downcast_ref::<Refused>() {
        return refused.code;
    }
    if error.downcast_ref::<LinkDown>().is_some() {
        return ErrorCode::Unavailable;
    }
    ErrorCode::Internal
}

pub fn answer(code: ErrorCode, error: String) -> Response {
    let status = StatusCode::from_u16(code.http_status()).expect("error codes map to valid statuses");
    (status, Json(ApiError { code, error })).into_response()
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let code = code_of(&self.0);
        if code == ErrorCode::Internal {
            eprintln!("error: API: {:#}", self.0);
        }
        answer(code, format!("{:#}", self.0))
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn a_mark_survives_added_context_and_unmarked_errors_are_internal() {
        let marked = Err::<(), _>(bad_request("no option x")).context("widget 1").unwrap_err();
        assert_eq!(code_of(&marked), ErrorCode::BadRequest);
        assert_eq!(format!("{marked:#}"), "widget 1: no option x");
        let down = anyhow::Error::from(LinkDown).context("deploying");
        assert_eq!(code_of(&down), ErrorCode::Unavailable);
        assert_eq!(code_of(&anyhow::anyhow!("disk full")), ErrorCode::Internal);
    }
}
