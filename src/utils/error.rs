use std::convert::Infallible;

use http::StatusCode;
use ruma::{
    api::client::{
        error::{Error as RumaError, ErrorBody, ErrorKind},
        uiaa::{UiaaInfo, UiaaResponse},
    },
    OwnedServerName,
};
use thiserror::Error;
use tracing::{error, info};

use crate::RumaResponse;

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Error, Debug)]
#[allow(clippy::error_impl_error)]
pub(crate) enum Error {
    #[cfg(feature = "sqlite")]
    #[error("There was a problem with the connection to the sqlite database: {source}")]
    Sqlite {
        #[from]
        source: rusqlite::Error,
    },
    #[cfg(feature = "rocksdb")]
    #[error("There was a problem with the connection to the rocksdb database: {source}")]
    RocksDb {
        #[from]
        source: rocksdb::Error,
    },
    #[error("Could not generate an image.")]
    Image {
        #[from]
        source: image::error::ImageError,
    },
    #[error("Could not connect to server: {source}")]
    Reqwest {
        #[from]
        source: reqwest::Error,
    },
    #[error("Could build regular expression: {source}")]
    Regex {
        #[from]
        source: regex::Error,
    },
    #[error("{0}")]
    Federation(OwnedServerName, RumaError),
    #[error("Could not do this io: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
    #[error("{0}")]
    BadServerResponse(&'static str),
    #[error("{0}")]
    BadConfig(&'static str),
    #[error("{0}")]
    /// Don't create this directly. Use Error::bad_database instead.
    BadDatabase(&'static str),
    #[error("uiaa")]
    Uiaa(UiaaInfo),
    #[error("{0}: {1}")]
    BadRequest(ErrorKind, &'static str),
    // This is only needed for when a room alias already exists
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Extension(#[from] axum::extract::rejection::ExtensionRejection),
    #[error("{0}")]
    Path(#[from] axum::extract::rejection::PathRejection),
    #[error("{0}")]
    AdminCommand(&'static str),
    #[error("from {0}: {1}")]
    Redaction(OwnedServerName, ruma::canonical_json::RedactionError),
    #[error("{0} in {1}")]
    InconsistentRoomState(&'static str, ruma::OwnedRoomId),
}

impl Error {
    pub(crate) fn bad_database(message: &'static str) -> Self {
        error!("BadDatabase: {}", message);
        Self::BadDatabase(message)
    }

    pub(crate) fn bad_config(message: &'static str) -> Self {
        error!("BadConfig: {}", message);
        Self::BadConfig(message)
    }

    pub(crate) fn to_response(&self) -> RumaResponse<UiaaResponse> {
        use ErrorKind::{
            Forbidden, GuestAccessForbidden, LimitExceeded, MissingToken, NotFound,
            ThreepidAuthFailed, ThreepidDenied, TooLarge, Unauthorized, Unknown, UnknownToken,
            Unrecognized, UserDeactivated, WrongRoomKeysVersion,
        };

        if let Self::Uiaa(uiaainfo) = self {
            return RumaResponse(UiaaResponse::AuthResponse(uiaainfo.clone()));
        }

        if let Self::Federation(origin, error) = self {
            let mut error = error.clone();
            error.body = ErrorBody::Standard {
                kind: Unknown,
                message: format!("Answer from {origin}: {error}"),
            };
            return RumaResponse(UiaaResponse::MatrixError(error));
        }

        let message = format!("{self}");

        let (kind, status_code) = match self {
            Self::BadRequest(kind, _) => (
                kind.clone(),
                match kind {
                    WrongRoomKeysVersion { .. }
                    | Forbidden
                    | GuestAccessForbidden
                    | ThreepidAuthFailed
                    | UserDeactivated
                    | ThreepidDenied => StatusCode::FORBIDDEN,
                    Unauthorized | UnknownToken { .. } | MissingToken => StatusCode::UNAUTHORIZED,
                    NotFound | Unrecognized => StatusCode::NOT_FOUND,
                    LimitExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
                    TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                    _ => StatusCode::BAD_REQUEST,
                },
            ),
            Self::Conflict(_) => (Unknown, StatusCode::CONFLICT),
            _ => (Unknown, StatusCode::INTERNAL_SERVER_ERROR),
        };

        info!("Returning an error: {}: {}", status_code, message);

        RumaResponse(UiaaResponse::MatrixError(RumaError {
            body: ErrorBody::Standard { kind, message },
            status_code,
        }))
    }

    /// Sanitizes public-facing errors that can leak sensitive information.
    pub(crate) fn sanitized_error(&self) -> String {
        let db_error = String::from("Database or I/O error occurred.");

        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite { .. } => db_error,
            #[cfg(feature = "rocksdb")]
            Self::RocksDb { .. } => db_error,
            Self::Io { .. } => db_error,
            Self::BadConfig { .. } => db_error,
            Self::BadDatabase { .. } => db_error,
            _ => self.to_string(),
        }
    }
}

impl From<Infallible> for Error {
    fn from(i: Infallible) -> Self {
        match i {}
    }
}

impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        self.to_response().into_response()
    }
}
