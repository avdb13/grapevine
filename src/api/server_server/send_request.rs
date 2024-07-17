use std::{fmt::Debug, mem};

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

use super::resolution::find_actual_destination;
use crate::{
    observability::{FoundIn, Lookup, METRICS},
    services,
    utils::dbg_truncate_str,
    Error, Result,
};

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

    let resolution = if let Some(result) = cached_result {
        METRICS.record_lookup(Lookup::FederationDestination, FoundIn::Cache);
        result
    } else {
        write_destination_to_cache = true;

        find_actual_destination(destination).await
    };

    let base_url = resolution.base_url();

    let http_request = request
        .try_into_http_request::<Vec<u8>>(
            &base_url,
            SendAccessToken::IfRequired(""),
            &[MatrixVersion::V1_4],
        )
        .map_err(|error| {
            warn!(
                %error,
                base_url,
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
        services()
            .globals
            .actual_destination_cache
            .write()
            .await
            .insert(OwnedServerName::from(destination), resolution);
    }

    response.map_err(|e| {
        warn!(error = %e, "Invalid 200 response");
        Error::BadServerResponse("Server returned bad 200 response.")
    })
}
