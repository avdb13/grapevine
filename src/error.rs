//! Error handling facilities

use std::{fmt, iter, path::PathBuf};

use thiserror::Error;

use crate::config::ListenConfig;

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
    #[error("failed to load configuration")]
    Config(#[from] Config),

    #[error("failed to initialize observability")]
    Observability(#[from] Observability),

    #[error("failed to load or create the database")]
    DatabaseError(#[source] crate::utils::error::Error),

    #[error("failed to serve requests")]
    Serve(#[from] Serve),
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

    #[error("failed to install global default tracing subscriber")]
    SetSubscriber(#[from] tracing::subscriber::SetGlobalDefaultError),

    // Upstream's documentation on what this error means is very sparse
    #[error("tracing_flame error")]
    TracingFlame(#[from] tracing_flame::Error),
}

/// Configuration errors
// Missing docs are allowed here since that kind of information should be
// encoded in the error messages themselves anyway.
#[allow(missing_docs)]
#[derive(Error, Debug)]
pub(crate) enum Config {
    #[error("failed to find configuration file")]
    Search(#[from] ConfigSearch),

    #[error("failed to read configuration file {1:?}")]
    Read(#[source] std::io::Error, PathBuf),

    #[error("failed to parse configuration file {1:?}")]
    Parse(#[source] toml::de::Error, PathBuf),
}

/// Errors that can occur while searching for a config file
// Missing docs are allowed here since that kind of information should be
// encoded in the error messages themselves anyway.
#[allow(missing_docs)]
#[derive(Error, Debug)]
pub(crate) enum ConfigSearch {
    #[error("XDG Base Directory error")]
    Xdg(#[from] xdg::BaseDirectoriesError),

    #[error("no relevant configuration files found in XDG Base Directories")]
    NotFound,
}

/// Errors serving traffic
// Missing docs are allowed here since that kind of information should be
// encoded in the error messages themselves anyway.
#[allow(missing_docs)]
#[derive(Error, Debug)]
pub(crate) enum Serve {
    #[error("no listeners were specified in the configuration file")]
    NoListeners,

    #[error(
        "listener {0} requested TLS, but no TLS cert was specified in the \
         configuration file. Please set 'tls.certs' and 'tls.key'"
    )]
    NoTlsCerts(ListenConfig),

    #[error("failed to read TLS cert and key files at {certs:?} and {key:?}")]
    LoadCerts {
        certs: String,
        key: String,
        #[source]
        err: std::io::Error,
    },

    #[error("failed to run request listener on {1}")]
    Listen(#[source] std::io::Error, ListenConfig),
}
