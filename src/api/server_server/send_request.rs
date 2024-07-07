use std::{
    fmt::Debug,
    mem,
    net::{IpAddr, SocketAddr},
};

use axum_extra::headers::{Authorization, HeaderMapExt};
use bytes::Bytes;
use ruma::{
    api::{
        client::error::Error as RumaError, EndpointError, IncomingResponse,
        MatrixVersion, Metadata, OutgoingRequest, SendAccessToken,
    },
    server_util::authorization::XMatrix,
    CanonicalJsonObject, OwnedServerName, OwnedSigningKeyId, ServerName,
};
use thiserror::Error;
use tracing::{debug, error, field, warn};

use crate::{
    observability::{FoundIn, Lookup, METRICS},
    services,
    utils::dbg_truncate_str,
    Error, Result,
};

/// Wraps either an literal IP address plus port, or a hostname plus complement
/// (colon-plus-port if it was specified).
///
/// Note: A [`FedDest::Named`] might contain an IP address in string form if
/// there was no port specified to construct a [`SocketAddr`] with.
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
pub(crate) enum FedDest {
    Literal(SocketAddr),
    Named(String, String),
}

impl FedDest {
    fn to_https_string(&self) -> String {
        match self {
            Self::Literal(addr) => format!("https://{addr}"),
            Self::Named(host, port) => format!("https://{host}{port}"),
        }
    }

    fn to_uri_string(&self) -> String {
        match self {
            Self::Literal(addr) => addr.to_string(),
            Self::Named(host, port) => format!("{host}{port}"),
        }
    }

    fn hostname(&self) -> String {
        match &self {
            Self::Literal(addr) => addr.ip().to_string(),
            Self::Named(host, _) => host.clone(),
        }
    }

    fn port(&self) -> Option<u16> {
        match &self {
            Self::Literal(addr) => Some(addr.port()),
            Self::Named(_, port) => {
                port.strip_prefix(':').and_then(|x| x.parse().ok())
            }
        }
    }
}

#[derive(Debug, Error)]
enum RequestSignError {
    #[error("invalid JSON in request body")]
    InvalidBodyJson(#[source] serde_json::Error),
    #[error("request has no path")]
    NoPath,
}

#[tracing::instrument(skip(http_request, metadata))]
fn create_request_signature(
    http_request: &http::Request<Vec<u8>>,
    metadata: &Metadata,
    destination: OwnedServerName,
) -> Result<Authorization<XMatrix>, RequestSignError> {
    let mut request_map = CanonicalJsonObject::new();

    if !http_request.body().is_empty() {
        request_map.insert(
            "content".to_owned(),
            serde_json::from_slice(http_request.body())
                .map_err(RequestSignError::InvalidBodyJson)?,
        );
    };

    request_map.insert("method".to_owned(), metadata.method.to_string().into());
    request_map.insert(
        "uri".to_owned(),
        http_request
            .uri()
            .path_and_query()
            .ok_or(RequestSignError::NoPath)?
            .to_string()
            .into(),
    );
    request_map.insert(
        "origin".to_owned(),
        services().globals.server_name().as_str().into(),
    );
    request_map.insert("destination".to_owned(), destination.as_str().into());

    ruma::signatures::sign_json(
        services().globals.server_name().as_str(),
        services().globals.keypair(),
        &mut request_map,
    )
    .expect("our request json is what ruma expects");

    // There's exactly the one signature we just created, fish it back out again
    let (key_id, signature) = request_map["signatures"]
        .as_object()
        .unwrap()
        .get(services().globals.server_name().as_str())
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .next()
        .unwrap();

    let key_id = OwnedSigningKeyId::try_from(key_id.clone()).unwrap();
    let signature = signature.as_str().unwrap().to_owned();

    Ok(Authorization(XMatrix::new(
        services().globals.server_name().to_owned(),
        Some(destination),
        key_id,
        signature,
    )))
}

/// Inner non-generic part of [`send_request()`] to reduce monomorphization
/// bloat
///
/// Takes an [`http::Request`], converts it to a [`reqwest::Request`], then
/// converts the [`reqwest::Response`] back to an [`http::Response`].
async fn send_request_inner(
    mut http_request: http::Request<Vec<u8>>,
    metadata: &Metadata,
    destination: OwnedServerName,
    log_error: bool,
) -> Result<http::Response<Bytes>> {
    let signature =
        create_request_signature(&http_request, metadata, destination.clone())
            .expect("all outgoing requests can be signed");
    http_request.headers_mut().typed_insert(signature);

    let reqwest_request = reqwest::Request::try_from(http_request)?;

    let url = reqwest_request.url().clone();
    tracing::Span::current().record("url", field::display(url));

    debug!("Sending request");
    let response =
        services().globals.federation_client().execute(reqwest_request).await;

    let mut response = response.inspect_err(|error| {
        if log_error {
            warn!(%error, "Could not send request");
        }
    })?;

    // reqwest::Response -> http::Response conversion
    let status = response.status();
    debug!(status = u16::from(status), "Received response");
    let mut http_response_builder =
        http::Response::builder().status(status).version(response.version());
    mem::swap(
        response.headers_mut(),
        http_response_builder
            .headers_mut()
            .expect("http::response::Builder is usable"),
    );

    debug!("Getting response bytes");
    // TODO: handle timeout
    let body = response.bytes().await.unwrap_or_else(|error| {
        warn!(%error, "Server error");
        Vec::new().into()
    });
    debug!("Got response bytes");

    if status != 200 {
        warn!(
            status = u16::from(status),
            response =
                dbg_truncate_str(String::from_utf8_lossy(&body).as_ref(), 100)
                    .into_owned(),
            "Received error over federation",
        );
    }

    let http_response = http_response_builder
        .body(body)
        .expect("reqwest body is valid http body");

    if status != 200 {
        return Err(Error::Federation(
            destination,
            RumaError::from_http_response(http_response),
        ));
    }

    Ok(http_response)
}

#[tracing::instrument(skip(request, log_error), fields(url))]
pub(crate) async fn send_request<T>(
    destination: &ServerName,
    request: T,
    log_error: bool,
) -> Result<T::IncomingResponse>
where
    T: OutgoingRequest + Debug,
{
    if !services().globals.allow_federation() {
        return Err(Error::BadConfig("Federation is disabled."));
    }

    if destination == services().globals.server_name() {
        return Err(Error::bad_config(
            "Won't send federation request to ourselves",
        ));
    }

    debug!("Preparing to send request");

    let mut write_destination_to_cache = false;

    let cached_result = services()
        .globals
        .actual_destination_cache
        .read()
        .await
        .get(destination)
        .cloned();

    let (actual_destination, host) = if let Some(result) = cached_result {
        METRICS.record_lookup(Lookup::FederationDestination, FoundIn::Cache);
        result
    } else {
        write_destination_to_cache = true;

        let result = find_actual_destination(destination).await;

        (result.0, result.1.to_uri_string())
    };

    let actual_destination_str = actual_destination.to_https_string();

    let http_request = request
        .try_into_http_request::<Vec<u8>>(
            &actual_destination_str,
            SendAccessToken::IfRequired(""),
            &[MatrixVersion::V1_4],
        )
        .map_err(|error| {
            warn!(
                %error,
                actual_destination = actual_destination_str,
                "Failed to serialize request",
            );
            Error::BadServerResponse("Invalid request")
        })?;

    let http_response = send_request_inner(
        http_request,
        &T::METADATA,
        destination.to_owned(),
        log_error,
    )
    .await?;

    debug!("Parsing response bytes");
    let response = T::IncomingResponse::try_from_http_response(http_response);
    if response.is_ok() && write_destination_to_cache {
        METRICS.record_lookup(Lookup::FederationDestination, FoundIn::Remote);
        services().globals.actual_destination_cache.write().await.insert(
            OwnedServerName::from(destination),
            (actual_destination, host),
        );
    }

    response.map_err(|e| {
        warn!(error = %e, "Invalid 200 response");
        Error::BadServerResponse("Server returned bad 200 response.")
    })
}

fn get_ip_with_port(destination_str: &str) -> Option<FedDest> {
    if let Ok(destination) = destination_str.parse::<SocketAddr>() {
        Some(FedDest::Literal(destination))
    } else if let Ok(ip_addr) = destination_str.parse::<IpAddr>() {
        Some(FedDest::Literal(SocketAddr::new(ip_addr, 8448)))
    } else {
        None
    }
}

fn add_port_to_hostname(destination_str: &str) -> FedDest {
    let (host, port) = match destination_str.find(':') {
        None => (destination_str, ":8448"),
        Some(pos) => destination_str.split_at(pos),
    };
    FedDest::Named(host.to_owned(), port.to_owned())
}

/// Returns: `actual_destination`, `Host` header
/// Implemented according to the specification at <https://matrix.org/docs/spec/server_server/r0.1.4#resolving-server-names>
/// Numbers in comments below refer to bullet points in linked section of
/// specification
#[allow(clippy::too_many_lines)]
#[tracing::instrument(ret(level = "debug"))]
async fn find_actual_destination(
    destination: &'_ ServerName,
) -> (FedDest, FedDest) {
    debug!("Finding actual destination");
    let destination_str = destination.as_str().to_owned();
    let mut hostname = destination_str.clone();
    let actual_destination = match get_ip_with_port(&destination_str) {
        Some(host_port) => {
            debug!("1: IP literal with provided or default port");
            host_port
        }
        None => {
            if let Some(pos) = destination_str.find(':') {
                debug!("2: Hostname with included port");
                let (host, port) = destination_str.split_at(pos);
                FedDest::Named(host.to_owned(), port.to_owned())
            } else {
                debug!(%destination, "Requesting well known");
                if let Some(delegated_hostname) =
                    request_well_known(destination.as_str()).await
                {
                    debug!("3: A .well-known file is available");
                    hostname = add_port_to_hostname(&delegated_hostname)
                        .to_uri_string();
                    if let Some(host_and_port) =
                        get_ip_with_port(&delegated_hostname)
                    {
                        host_and_port
                    } else if let Some(pos) = delegated_hostname.find(':') {
                        debug!("3.2: Hostname with port in .well-known file");
                        let (host, port) = delegated_hostname.split_at(pos);
                        FedDest::Named(host.to_owned(), port.to_owned())
                    } else {
                        debug!("Delegated hostname has no port in this branch");
                        if let Some(hostname_override) =
                            query_srv_record(&delegated_hostname).await
                        {
                            debug!("3.3: SRV lookup successful");
                            let force_port = hostname_override.port();

                            if let Ok(override_ip) = services()
                                .globals
                                .dns_resolver()
                                .lookup_ip(hostname_override.hostname())
                                .await
                            {
                                services()
                                    .globals
                                    .tls_name_override
                                    .write()
                                    .unwrap()
                                    .insert(
                                        delegated_hostname.clone(),
                                        (
                                            override_ip.iter().collect(),
                                            force_port.unwrap_or(8448),
                                        ),
                                    );
                            } else {
                                warn!(
                                    "Using SRV record, but could not resolve \
                                     to IP"
                                );
                            }

                            if let Some(port) = force_port {
                                FedDest::Named(
                                    delegated_hostname,
                                    format!(":{port}"),
                                )
                            } else {
                                add_port_to_hostname(&delegated_hostname)
                            }
                        } else {
                            debug!(
                                "3.4: No SRV records, just use the hostname \
                                 from .well-known"
                            );
                            add_port_to_hostname(&delegated_hostname)
                        }
                    }
                } else {
                    debug!("4: No .well-known or an error occured");
                    if let Some(hostname_override) =
                        query_srv_record(&destination_str).await
                    {
                        debug!("4: SRV record found");
                        let force_port = hostname_override.port();

                        if let Ok(override_ip) = services()
                            .globals
                            .dns_resolver()
                            .lookup_ip(hostname_override.hostname())
                            .await
                        {
                            services()
                                .globals
                                .tls_name_override
                                .write()
                                .unwrap()
                                .insert(
                                    hostname.clone(),
                                    (
                                        override_ip.iter().collect(),
                                        force_port.unwrap_or(8448),
                                    ),
                                );
                        } else {
                            warn!(
                                "Using SRV record, but could not resolve to IP"
                            );
                        }

                        if let Some(port) = force_port {
                            FedDest::Named(hostname.clone(), format!(":{port}"))
                        } else {
                            add_port_to_hostname(&hostname)
                        }
                    } else {
                        debug!("5: No SRV record found");
                        add_port_to_hostname(&destination_str)
                    }
                }
            }
        }
    };
    debug!(?actual_destination, "Resolved actual destination");

    // Can't use get_ip_with_port here because we don't want to add a port
    // to an IP address if it wasn't specified
    let hostname = if let Ok(addr) = hostname.parse::<SocketAddr>() {
        FedDest::Literal(addr)
    } else if let Ok(addr) = hostname.parse::<IpAddr>() {
        FedDest::Named(addr.to_string(), ":8448".to_owned())
    } else if let Some(pos) = hostname.find(':') {
        let (host, port) = hostname.split_at(pos);
        FedDest::Named(host.to_owned(), port.to_owned())
    } else {
        FedDest::Named(hostname, ":8448".to_owned())
    };
    (actual_destination, hostname)
}

#[tracing::instrument(ret(level = "debug"))]
async fn query_given_srv_record(record: &str) -> Option<FedDest> {
    services()
        .globals
        .dns_resolver()
        .srv_lookup(record)
        .await
        .map(|srv| {
            srv.iter().next().map(|result| {
                FedDest::Named(
                    result
                        .target()
                        .to_string()
                        .trim_end_matches('.')
                        .to_owned(),
                    format!(":{}", result.port()),
                )
            })
        })
        .unwrap_or(None)
}

#[tracing::instrument(ret(level = "debug"))]
async fn query_srv_record(hostname: &'_ str) -> Option<FedDest> {
    let hostname = hostname.trim_end_matches('.');

    if let Some(host_port) =
        query_given_srv_record(&format!("_matrix-fed._tcp.{hostname}.")).await
    {
        Some(host_port)
    } else {
        query_given_srv_record(&format!("_matrix._tcp.{hostname}.")).await
    }
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
    if let Err(error) = &response {
        debug!(%error, "Failed to request .well-known");
        return None;
    }
    let text = response.ok()?.text().await;
    debug!("Got well known response text");
    let body: serde_json::Value = serde_json::from_str(&text.ok()?).ok()?;
    Some(body.get("m.server")?.as_str()?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{add_port_to_hostname, get_ip_with_port, FedDest};

    #[test]
    fn ips_get_default_ports() {
        assert_eq!(
            get_ip_with_port("1.1.1.1"),
            Some(FedDest::Literal("1.1.1.1:8448".parse().unwrap()))
        );
        assert_eq!(
            get_ip_with_port("dead:beef::"),
            Some(FedDest::Literal("[dead:beef::]:8448".parse().unwrap()))
        );
    }

    #[test]
    fn ips_keep_custom_ports() {
        assert_eq!(
            get_ip_with_port("1.1.1.1:1234"),
            Some(FedDest::Literal("1.1.1.1:1234".parse().unwrap()))
        );
        assert_eq!(
            get_ip_with_port("[dead::beef]:8933"),
            Some(FedDest::Literal("[dead::beef]:8933".parse().unwrap()))
        );
    }

    #[test]
    fn hostnames_get_default_ports() {
        assert_eq!(
            add_port_to_hostname("example.com"),
            FedDest::Named(String::from("example.com"), String::from(":8448"))
        );
    }

    #[test]
    fn hostnames_keep_custom_ports() {
        assert_eq!(
            add_port_to_hostname("example.com:1337"),
            FedDest::Named(String::from("example.com"), String::from(":1337"))
        );
    }
}
