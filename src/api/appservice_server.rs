use std::{fmt::Debug, mem, time::Duration};

use bytes::BytesMut;
use ruma::api::{
    appservice::Registration, IncomingResponse, MatrixVersion, OutgoingRequest,
    SendAccessToken,
};
use tracing::warn;

use crate::{services, utils, Error, Result};

/// Sends a request to an appservice
///
/// Only returns None if there is no url specified in the appservice
/// registration file
#[tracing::instrument(skip(request))]
pub(crate) async fn send_request<T>(
    registration: Registration,
    request: T,
) -> Result<Option<T::IncomingResponse>>
where
    T: OutgoingRequest + Debug,
{
    let Some(destination) = registration.url else {
        return Ok(None);
    };

    let hs_token = registration.hs_token.as_str();

    let mut http_request = request
        .try_into_http_request::<BytesMut>(
            &destination,
            SendAccessToken::IfRequired(hs_token),
            &[MatrixVersion::V1_0],
        )
        .unwrap()
        .map(BytesMut::freeze);

    let mut parts = http_request.uri().clone().into_parts();
    let old_path_and_query = parts.path_and_query.unwrap().as_str().to_owned();
    let symbol = if old_path_and_query.contains('?') {
        "&"
    } else {
        "?"
    };

    parts.path_and_query = Some(
        format!("{old_path_and_query}{symbol}access_token={hs_token}")
            .parse()
            .unwrap(),
    );
    *http_request.uri_mut() =
        parts.try_into().expect("our manipulation is always valid");

    let mut reqwest_request = reqwest::Request::try_from(http_request)?;

    *reqwest_request.timeout_mut() = Some(Duration::from_secs(30));

    let url = reqwest_request.url().clone();
    let mut response = services()
        .globals
        .default_client()
        .execute(reqwest_request)
        .await
        .inspect_err(|error| {
            warn!(
                %error,
                appservice = registration.id,
                %destination,
                "Could not send request to appservice",
            );
        })?;

    // reqwest::Response -> http::Response conversion
    let status = response.status();
    let mut http_response_builder =
        http::Response::builder().status(status).version(response.version());
    mem::swap(
        response.headers_mut(),
        http_response_builder
            .headers_mut()
            .expect("http::response::Builder is usable"),
    );

    // TODO: handle timeout
    let body = response.bytes().await.unwrap_or_else(|error| {
        warn!(%error, "Server error");
        Vec::new().into()
    });

    if status != 200 {
        warn!(
            appservice = %destination,
            %status,
            %url,
            body = %utils::dbg_truncate_str(
                String::from_utf8_lossy(&body).as_ref(),
                100,
            ),
            "Appservice returned bad response",
        );
    }

    let response = T::IncomingResponse::try_from_http_response(
        http_response_builder
            .body(body)
            .expect("reqwest body is valid http body"),
    );

    response.map(Some).map_err(|error| {
        warn!(
            %error,
            appservice = %destination,
            %url,
            "Appservice returned invalid response bytes",
        );
        Error::BadServerResponse("Server returned bad response.")
    })
}
