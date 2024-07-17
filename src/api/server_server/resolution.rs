use std::{
    borrow::Cow,
    fmt::Debug,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use ruma::ServerName;
use thiserror::Error;
use tracing::{debug, error, warn};

use crate::{services, Result};
/// Wraps either a literal IP address or a hostname, plus an optional port.
///
/// # Examples:
/// ```rust
/// # use grapevine::api::server_server::FedDest;
/// # fn main() -> Result<(), std::net::AddrParseError> {
/// FedDest::Literal("198.51.100.3:8448".parse()?);
/// FedDest::Literal("[2001:db8::4:5]:443".parse()?);
/// FedDest::Named("matrix.example.org".to_owned(), "".to_owned());
/// FedDest::Named("matrix.example.org".to_owned(), ":8448".to_owned());
/// FedDest::Named("198.51.100.5".to_owned(), "".to_owned());
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
enum FedDest {
    BareLiteral(IpAddr),
    PortLiteral(SocketAddr),
    Named(String, Option<u16>),
}

impl FedDest {
    fn host_and_port_or_default(&self) -> (Cow<'_, str>, u16) {
        const DEFAULT_PORT: u16 = 8448;

        match self {
            FedDest::BareLiteral(addr) => {
                (Cow::Owned(addr.to_string()), DEFAULT_PORT)
            }
            FedDest::PortLiteral(addr) => {
                (Cow::Owned(addr.ip().to_string()), addr.port())
            }
            FedDest::Named(host, port) => {
                (Cow::Borrowed(host), port.unwrap_or(DEFAULT_PORT))
            }
        }
    }
}

#[derive(Debug, Error)]
enum InvalidFedDest {
    #[error("invalid port {0}")]
    InvalidPort(String),
}

impl FromStr for FedDest {
    type Err = InvalidFedDest;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(destination) = s.parse::<SocketAddr>() {
            Ok(FedDest::PortLiteral(destination))
        } else if let Ok(ip_addr) = s.parse::<IpAddr>() {
            Ok(FedDest::BareLiteral(ip_addr))
        } else if let Some((host, port)) = s.split_once(':') {
            Ok(FedDest::Named(
                host.to_owned(),
                Some(port.parse().map_err(|_| {
                    InvalidFedDest::InvalidPort(port.to_owned())
                })?),
            ))
        } else {
            Ok(FedDest::Named(s.to_owned(), None))
        }
    }
}

#[derive(Debug, Clone)]
enum WellKnownResult {
    Success {
        delegated_dest: FedDest,
    },
    Error,
}

#[derive(Debug, Clone)]
enum SrvResult {
    Success {
        host: String,
        port: u16,
    },
    Error,
}

#[derive(Debug, Clone)]
struct LookupResult {
    well_known: WellKnownResult,
    srv: Option<SrvResult>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolutionResult {
    original: FedDest,
    // None if `original` is an IP literal or has a port (used as-is)
    lookup: Option<LookupResult>,
}

impl ResolutionResult {
    pub(crate) fn host_header(&self) -> String {
        let dest = match &self.lookup {
            None
            | Some(LookupResult {
                well_known: WellKnownResult::Error,
                ..
            }) => {
                // no lookup performed or well-known lookup failed
                &self.original
            }

            Some(LookupResult {
                well_known:
                    WellKnownResult::Success {
                        delegated_dest,
                    },
                ..
            }) => {
                // well-known lookup succeeded
                delegated_dest
            }
        };

        match dest {
            FedDest::BareLiteral(addr) => format!("{addr}"),
            FedDest::PortLiteral(addr) => format!("{addr}"),
            FedDest::Named(host, port) => {
                if let Some(port) = port {
                    format!("{host}:{port}")
                } else {
                    host.clone()
                }
            }
        }
    }

    pub(crate) fn base_url(&self) -> String {
        let (host, port) = match &self.lookup {
            None
            | Some(LookupResult {
                well_known: WellKnownResult::Error,
                srv: None | Some(SrvResult::Error),
            }) => {
                // all lookups failed, or no lookups were performed
                self.original.host_and_port_or_default()
            }

            Some(LookupResult {
                well_known:
                    WellKnownResult::Success {
                        delegated_dest,
                    },
                srv: None | Some(SrvResult::Error),
            }) => {
                // SRV lookup failed, but well-known lookup succeeded
                delegated_dest.host_and_port_or_default()
            }

            Some(LookupResult {
                srv:
                    Some(SrvResult::Success {
                        host,
                        port,
                    }),
                ..
            }) => {
                // SRV lookup succeeded (result of well-known lookup isn't
                // relevant)
                (Cow::Borrowed(host.as_str()), *port)
            }
        };

        format!("https://{host}:{port}")
    }
}

/// Returns: `actual_destination`, `Host` header
/// Implemented according to the specification at <https://matrix.org/docs/spec/server_server/r0.1.4#resolving-server-names>
/// Numbers in comments below refer to bullet points in linked section of
/// specification
#[allow(clippy::too_many_lines)]
#[tracing::instrument(ret(level = "debug"))]
pub(crate) async fn find_actual_destination(
    destination: &'_ ServerName,
) -> ResolutionResult {
    let original: FedDest = destination
        .as_str()
        .parse()
        .expect("ServerName should always be a valid FedDest");

    let hostname = match &original {
        FedDest::BareLiteral(_) | FedDest::PortLiteral(_) => {
            debug!("1: IP literal");
            return ResolutionResult {
                original,
                lookup: None,
            };
        }
        FedDest::Named(_, Some(_)) => {
            debug!("2: Hostname with port");
            return ResolutionResult {
                original,
                lookup: None,
            };
        }
        FedDest::Named(host, None) => host,
    };

    debug!("Requesting .well-known");
    let well_known_error = 'well_known: {
        let Some(delegated_hostname) = request_well_known(hostname).await
        else {
            debug!("Invalid/failed .well-known response");
            break 'well_known WellKnownResult::Error;
        };
        let Ok(delegated_dest) = delegated_hostname.parse() else {
            debug!("Malformed delegation in .well-known");
            break 'well_known WellKnownResult::Error;
        };

        debug!("3: A .well-known file is available");

        let srv = match &delegated_dest {
            FedDest::BareLiteral(_) | FedDest::PortLiteral(_) => {
                debug!("3.1: IP literal in .well-known file");

                None
            }
            FedDest::Named(_, Some(_)) => {
                debug!("3.2: Hostname with port in .well-known file");

                None
            }
            FedDest::Named(delegated_hostname, None) => {
                let srv = query_and_store_srv_record(delegated_hostname).await;
                if let SrvResult::Success {
                    ..
                } = &srv
                {
                    debug!(
                        "3.3/3.4: SRV lookup of delegated destination \
                         successful"
                    );
                } else {
                    debug!(
                        "3.5: SRV lookup failed, using delegated destination"
                    );
                }

                Some(srv)
            }
        };

        return ResolutionResult {
            original,
            lookup: Some(LookupResult {
                well_known: WellKnownResult::Success {
                    delegated_dest,
                },
                srv,
            }),
        };
    };

    let srv = query_and_store_srv_record(hostname).await;
    if let SrvResult::Success {
        ..
    } = &srv
    {
        debug!("4/5: SRV lookup of original destination successful");
    } else {
        debug!("6: SRV lookup failed, using original destination");
    }

    ResolutionResult {
        original,
        lookup: Some(LookupResult {
            well_known: well_known_error,
            srv: Some(srv),
        }),
    }
}

#[tracing::instrument(ret(level = "debug"))]
async fn query_given_srv_record(record: &str) -> SrvResult {
    services()
        .globals
        .dns_resolver()
        .srv_lookup(record)
        .await
        .ok()
        .and_then(|srv| {
            srv.iter().next().map(|result| SrvResult::Success {
                host: result
                    .target()
                    .to_string()
                    .trim_end_matches('.')
                    .to_owned(),
                port: result.port(),
            })
        })
        .unwrap_or(SrvResult::Error)
}

#[tracing::instrument(ret(level = "debug"))]
async fn query_and_store_srv_record(hostname: &'_ str) -> SrvResult {
    let hostname = hostname.trim_end_matches('.');

    let mut result =
        query_given_srv_record(&format!("_matrix-fed._tcp.{hostname}.")).await;
    if matches!(result, SrvResult::Error) {
        result =
            query_given_srv_record(&format!("_matrix._tcp.{hostname}.")).await;
    }

    let SrvResult::Success {
        host,
        port,
    } = &result
    else {
        return result;
    };

    if let Ok(override_ip) =
        services().globals.dns_resolver().lookup_ip(host).await
    {
        services()
            .globals
            .tls_name_override
            .write()
            .unwrap()
            .insert(hostname.to_owned(), (override_ip.iter().collect(), *port));
    } else {
        warn!("Using SRV record, but could not resolve to IP");
    }

    result
}

#[tracing::instrument(ret(level = "debug"))]
async fn request_well_known(destination: &str) -> Option<String> {
    let response = services()
        .globals
        .default_client()
        .get(&format!("https://{destination}/.well-known/matrix/server"))
        .send()
        .await;
    debug!("Got well known response");
    if let Err(e) = &response {
        debug!("Well known error: {e:?}");
        return None;
    }
    let text = response.ok()?.text().await;
    debug!("Got well known response text");
    let body: serde_json::Value = serde_json::from_str(&text.ok()?).ok()?;
    Some(body.get("m.server")?.as_str()?.to_owned())
}
