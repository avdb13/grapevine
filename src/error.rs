//! Error handling facilities

use std::{fmt, iter};

use thiserror::Error;

/// Formats an [`Error`][0] and its [`source`][1]s with a separator
///
/// [0]: std::error::Error
/// [1]: std::error::Error::source
pub(crate) struct DisplayWithSources<'a> {
    /// The error (and its sources) to write
    pub(crate) error: &'a dyn std::error::Error,

    /// Separator to write between the original error and subsequent sources
    pub(crate) infix: &'static str,
}

impl fmt::Display for DisplayWithSources<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)?;

        let mut source = self.error.source();

        source
            .into_iter()
            .chain(iter::from_fn(|| {
                source = source.and_then(std::error::Error::source);
                source
            }))
            .try_for_each(|source| write!(f, "{}{source}", self.infix))
    }
}

/// Top-level errors
// Missing docs are allowed here since that kind of information should be
// encoded in the error messages themselves anyway.
#[allow(missing_docs)]
#[derive(Error, Debug)]
pub(crate) enum Main {
    #[error("invalid configuration")]
    ConfigInvalid(#[from] figment::Error),

    #[error("failed to initialize observability")]
    Observability(#[from] Observability),

    #[error("failed to load or create the database")]
    DatabaseError(#[source] crate::utils::error::Error),

    #[error("failed to serve requests")]
    Serve(#[source] std::io::Error),
}

/// Observability initialization errors
// Missing docs are allowed here since that kind of information should be
// encoded in the error messages themselves anyway.
#[allow(missing_docs)]
#[derive(Error, Debug)]
pub(crate) enum Observability {
    // Upstream's documentation on what this error means is very sparse
    #[error("opentelemetry error")]
    Otel(#[from] opentelemetry::trace::TraceError),

    #[error("invalid log filter syntax")]
    EnvFilter(#[from] tracing_subscriber::filter::ParseError),

    #[error("failed to install global default tracing subscriber")]
    SetSubscriber(#[from] tracing::subscriber::SetGlobalDefaultError),

    // Upstream's documentation on what this error means is very sparse
    #[error("tracing_flame error")]
    TracingFlame(#[from] tracing_flame::Error),
}
