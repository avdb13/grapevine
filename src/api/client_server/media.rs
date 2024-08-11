use std::time::Duration;

use axum::response::IntoResponse;
use http::{
    header::{CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE},
    HeaderName, HeaderValue, Method,
};
use phf::{phf_set, Set};
use ruma::{
    api::{
        client::{
            authenticated_media as authenticated_media_client,
            error::ErrorKind,
            media::{self as legacy_media, create_content},
        },
        federation::authenticated_media as authenticated_media_fed,
    },
    http_headers::{ContentDisposition, ContentDispositionType},
};
use tracing::{debug, error, info, warn};

use crate::{
    service::media::FileMeta,
    services,
    utils::{self, MxcData},
    Ar, Error, Ra, Result,
};

const MXC_LENGTH: usize = 32;

/// `Content-Type`s that can be rendered inline in a browser without risking XSS
///
/// Cargo-culted from Synapse. Note that SVG can contain inline JavaScript.
static INLINE_CONTENT_TYPES: Set<&str> = phf_set! {
    // Keep sorted
    "application/json",
    "application/ld+json",
    "audio/aac",
    "audio/flac",
    "audio/mp4",
    "audio/mpeg",
    "audio/ogg",
    "audio/wav",
    "audio/wave",
    "audio/webm",
    "audio/x-flac",
    "audio/x-pn-wav",
    "audio/x-wav",
    "image/apng",
    "image/avif",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
    "text/css",
    "text/csv",
    "text/plain",
    "video/mp4",
    "video/ogg",
    "video/quicktime",
    "video/webm",
};

/// Value for the `Content-Security-Policy` header
///
/// Cargo-culted from Synapse.
fn content_security_policy() -> HeaderValue {
    [
        "sandbox",
        "default-src 'none'",
        "script-src 'none'",
        "plugin-types application/pdf",
        "style-src 'unsafe-inline'",
        "media-src 'self'",
        "object-src 'self'",
    ]
    .join("; ")
    .try_into()
    .expect("hardcoded header value should be valid")
}

/// Determine a `Content-Disposition` header that prevents XSS
// TODO: In some of the places this function is called, we could parse the
// desired filename out of an existing `Content-Disposition` header value, such
// as what we're storing in the database or what we receive over federation.
// Doing this correctly is tricky, so I'm skipping it for now.
fn content_disposition_for(
    content_type: Option<&str>,
    filename: Option<String>,
) -> ContentDisposition {
    let disposition_type = match content_type {
        Some(x) if INLINE_CONTENT_TYPES.contains(x) => {
            ContentDispositionType::Inline
        }
        _ => ContentDispositionType::Attachment,
    };
    ContentDisposition {
        disposition_type,
        filename,
    }
}

/// Set a header, but panic if it was already set
///
/// # Panics
///
/// Panics if the header was already set.
fn set_header_or_panic(
    response: &mut axum::response::Response,
    header_name: HeaderName,
    header_value: HeaderValue,
) {
    if let Some(header_value) = response.headers().get(&header_name) {
        error!(?header_name, ?header_value, "unexpected pre-existing header");
        panic!(
            "expected {header_name:?} to be unset but it was set to \
             {header_value:?}"
        );
    }

    response.headers_mut().insert(header_name, header_value);
}

/// # `GET /_matrix/media/r0/config`
///
/// Returns max upload size.
#[allow(deprecated)] // unauthenticated media
pub(crate) async fn get_media_config_legacy_route(
    _body: Ar<legacy_media::get_media_config::v3::Request>,
) -> Result<Ra<legacy_media::get_media_config::v3::Response>> {
    Ok(Ra(legacy_media::get_media_config::v3::Response {
        upload_size: services().globals.max_request_size().into(),
    }))
}

/// # `GET /_matrix/client/v1/media/config`
///
/// Returns max upload size.
pub(crate) async fn get_media_config_route(
    _body: Ar<authenticated_media_client::get_media_config::v1::Request>,
) -> Result<Ra<authenticated_media_client::get_media_config::v1::Response>> {
    Ok(Ra(authenticated_media_client::get_media_config::v1::Response {
        upload_size: services().globals.max_request_size().into(),
    }))
}

/// # `POST /_matrix/media/r0/upload`
///
/// Permanently save media in the server.
///
/// - Some metadata will be saved in the database
/// - Media will be saved in the media/ directory
pub(crate) async fn create_content_route(
    body: Ar<create_content::v3::Request>,
) -> Result<Ra<create_content::v3::Response>> {
    let media_id = utils::random_string(MXC_LENGTH);
    let mxc = MxcData::new(services().globals.server_name(), &media_id)?;

    services()
        .media
        .create(
            mxc.to_string(),
            body.filename
                .clone()
                .map(|filename| ContentDisposition {
                    disposition_type: ContentDispositionType::Inline,
                    filename: Some(filename),
                })
                .as_ref(),
            body.content_type.as_deref(),
            &body.file,
        )
        .await?;

    Ok(Ra(create_content::v3::Response {
        content_uri: mxc.into(),
        blurhash: None,
    }))
}

struct RemoteResponse {
    #[allow(unused)]
    metadata: authenticated_media_fed::ContentMetadata,
    content: authenticated_media_fed::Content,
}

/// Fetches remote media content from a URL specified in a
/// `/_matrix/federation/v1/media/*/{mediaId}` `Location` header
#[tracing::instrument]
async fn get_redirected_content(
    location: String,
) -> Result<authenticated_media_fed::Content> {
    let location = location.parse().map_err(|error| {
        warn!(location, %error, "Invalid redirect location");
        Error::BadServerResponse("Invalid redirect location")
    })?;
    let response = services()
        .globals
        .federation_client()
        .execute(reqwest::Request::new(Method::GET, location))
        .await?;

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .map(|value| {
            value.to_str().map_err(|error| {
                error!(
                    ?value,
                    %error,
                    "Invalid Content-Type header"
                );
                Error::BadServerResponse("Invalid Content-Type header")
            })
        })
        .transpose()?
        .map(str::to_owned);

    let content_disposition = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .map(|value| {
            ContentDisposition::try_from(value.as_bytes()).map_err(|error| {
                error!(
                    ?value,
                    %error,
                    "Invalid Content-Disposition header"
                );
                Error::BadServerResponse("Invalid Content-Disposition header")
            })
        })
        .transpose()?;

    Ok(authenticated_media_fed::Content {
        file: response.bytes().await?.to_vec(),
        content_type,
        content_disposition,
    })
}

#[tracing::instrument(skip_all)]
async fn get_remote_content_via_federation_api(
    mxc: &MxcData<'_>,
) -> Result<RemoteResponse, Error> {
    let authenticated_media_fed::get_content::v1::Response {
        metadata,
        content,
    } = services()
        .sending
        .send_federation_request(
            mxc.server_name,
            authenticated_media_fed::get_content::v1::Request {
                media_id: mxc.media_id.to_owned(),
                timeout_ms: Duration::from_secs(20),
            },
        )
        .await?;

    let content = match content {
        authenticated_media_fed::FileOrLocation::File(content) => {
            debug!("Got media from remote server");
            content
        }
        authenticated_media_fed::FileOrLocation::Location(location) => {
            debug!(location, "Following redirect");
            get_redirected_content(location).await?
        }
    };

    Ok(RemoteResponse {
        metadata,
        content,
    })
}

#[allow(deprecated)] // unauthenticated media
#[tracing::instrument(skip_all)]
async fn get_remote_content_via_legacy_api(
    mxc: &MxcData<'_>,
) -> Result<RemoteResponse, Error> {
    let content_response = services()
        .sending
        .send_federation_request(
            mxc.server_name,
            legacy_media::get_content::v3::Request {
                allow_remote: false,
                server_name: mxc.server_name.to_owned(),
                media_id: mxc.media_id.to_owned(),
                timeout_ms: Duration::from_secs(20),
                allow_redirect: false,
            },
        )
        .await?;

    Ok(RemoteResponse {
        metadata: authenticated_media_fed::ContentMetadata {},
        content: authenticated_media_fed::Content {
            file: content_response.file,
            content_disposition: content_response.content_disposition,
            content_type: content_response.content_type,
        },
    })
}

#[tracing::instrument]
pub(crate) async fn get_remote_content(
    mxc: &MxcData<'_>,
) -> Result<RemoteResponse, Error> {
    let fed_result = get_remote_content_via_federation_api(mxc).await;

    let response = match fed_result {
        Ok(response) => {
            debug!("Got remote content via authenticated media API");
            response
        }
        Err(Error::Federation(_, error))
            if error.error_kind() == Some(&ErrorKind::Unrecognized) =>
        {
            info!(
                "Remote server does not support authenticated media, falling \
                 back to deprecated API"
            );

            get_remote_content_via_legacy_api(mxc).await?
        }
        Err(e) => {
            return Err(e);
        }
    };

    services()
        .media
        .create(
            mxc.to_string(),
            response.content.content_disposition.as_ref(),
            response.content.content_type.as_deref(),
            &response.content.file,
        )
        .await?;

    Ok(response)
}

/// # `GET /_matrix/media/r0/download/{serverName}/{mediaId}`
///
/// Load media from our server or over federation.
///
/// - Only allows federation if `allow_remote` is true
#[allow(deprecated)] // unauthenticated media
pub(crate) async fn get_content_legacy_route(
    body: Ar<legacy_media::get_content::v3::Request>,
) -> Result<axum::response::Response> {
    use authenticated_media_client::get_content::v1::{
        Request as AmRequest, Response as AmResponse,
    };
    use legacy_media::get_content::v3::{
        Request as LegacyRequest, Response as LegacyResponse,
    };

    fn convert_request(
        LegacyRequest {
            server_name,
            media_id,
            timeout_ms,
            ..
        }: LegacyRequest,
    ) -> AmRequest {
        AmRequest {
            server_name,
            media_id,
            timeout_ms,
        }
    }

    fn convert_response(
        AmResponse {
            file,
            content_type,
            content_disposition,
        }: AmResponse,
    ) -> LegacyResponse {
        LegacyResponse {
            file,
            content_type,
            content_disposition,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        }
    }

    let allow_remote = body.allow_remote;

    get_content_route_ruma(body.map_body(convert_request), allow_remote)
        .await
        .map(|response| {
            let response = convert_response(response);
            let mut r = Ra(response).into_response();

            set_header_or_panic(
                &mut r,
                CONTENT_SECURITY_POLICY,
                content_security_policy(),
            );

            r
        })
}

/// # `GET /_matrix/client/v1/media/download/{serverName}/{mediaId}`
///
/// Load media from our server or over federation.
pub(crate) async fn get_content_route(
    body: Ar<authenticated_media_client::get_content::v1::Request>,
) -> Result<axum::response::Response> {
    get_content_route_ruma(body, true).await.map(|x| {
        let mut r = Ra(x).into_response();

        set_header_or_panic(
            &mut r,
            CONTENT_SECURITY_POLICY,
            content_security_policy(),
        );

        r
    })
}

async fn get_content_route_ruma(
    body: Ar<authenticated_media_client::get_content::v1::Request>,
    allow_remote: bool,
) -> Result<authenticated_media_client::get_content::v1::Response> {
    let mxc = MxcData::new(&body.server_name, &body.media_id)?;

    if let Some((
        FileMeta {
            content_type,
            ..
        },
        file,
    )) = services().media.get(mxc.to_string()).await?
    {
        Ok(authenticated_media_client::get_content::v1::Response {
            file,
            content_disposition: Some(content_disposition_for(
                content_type.as_deref(),
                None,
            )),
            content_type,
        })
    } else if &*body.server_name != services().globals.server_name()
        && allow_remote
    {
        let remote_response = get_remote_content(&mxc).await?;
        Ok(authenticated_media_client::get_content::v1::Response {
            file: remote_response.content.file,
            content_disposition: Some(content_disposition_for(
                remote_response.content.content_type.as_deref(),
                None,
            )),
            content_type: remote_response.content.content_type,
        })
    } else {
        Err(Error::BadRequest(ErrorKind::NotYetUploaded, "Media not found."))
    }
}

/// # `GET /_matrix/media/r0/download/{serverName}/{mediaId}/{fileName}`
///
/// Load media from our server or over federation, permitting desired filename.
///
/// - Only allows federation if `allow_remote` is true
#[allow(deprecated)] // unauthenticated media
pub(crate) async fn get_content_as_filename_legacy_route(
    body: Ar<legacy_media::get_content_as_filename::v3::Request>,
) -> Result<axum::response::Response> {
    use authenticated_media_client::get_content_as_filename::v1::{
        Request as AmRequest, Response as AmResponse,
    };
    use legacy_media::get_content_as_filename::v3::{
        Request as LegacyRequest, Response as LegacyResponse,
    };

    fn convert_request(
        LegacyRequest {
            server_name,
            media_id,
            filename,
            timeout_ms,
            ..
        }: LegacyRequest,
    ) -> AmRequest {
        AmRequest {
            server_name,
            media_id,
            filename,
            timeout_ms,
        }
    }

    fn convert_response(
        AmResponse {
            file,
            content_type,
            content_disposition,
        }: AmResponse,
    ) -> LegacyResponse {
        LegacyResponse {
            file,
            content_type,
            content_disposition,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        }
    }

    let allow_remote = body.allow_remote;
    get_content_as_filename_route_ruma(
        body.map_body(convert_request),
        allow_remote,
    )
    .await
    .map(|response| {
        let response = convert_response(response);
        let mut r = Ra(response).into_response();

        set_header_or_panic(
            &mut r,
            CONTENT_SECURITY_POLICY,
            content_security_policy(),
        );

        r
    })
}

/// # `GET /_matrix/client/v1/media/download/{serverName}/{mediaId}/{fileName}`
///
/// Load media from our server or over federation, permitting desired filename.
pub(crate) async fn get_content_as_filename_route(
    body: Ar<authenticated_media_client::get_content_as_filename::v1::Request>,
) -> Result<axum::response::Response> {
    get_content_as_filename_route_ruma(body, true).await.map(|x| {
        let mut r = Ra(x).into_response();

        set_header_or_panic(
            &mut r,
            CONTENT_SECURITY_POLICY,
            content_security_policy(),
        );

        r
    })
}

pub(crate) async fn get_content_as_filename_route_ruma(
    body: Ar<authenticated_media_client::get_content_as_filename::v1::Request>,
    allow_remote: bool,
) -> Result<authenticated_media_client::get_content_as_filename::v1::Response> {
    let mxc = MxcData::new(&body.server_name, &body.media_id)?;

    if let Some((
        FileMeta {
            content_type,
            ..
        },
        file,
    )) = services().media.get(mxc.to_string()).await?
    {
        Ok(authenticated_media_client::get_content_as_filename::v1::Response {
            file,
            content_disposition: Some(content_disposition_for(
                content_type.as_deref(),
                Some(body.filename.clone()),
            )),
            content_type,
        })
    } else if &*body.server_name != services().globals.server_name()
        && allow_remote
    {
        let remote_response = get_remote_content(&mxc).await?;

        Ok(authenticated_media_client::get_content_as_filename::v1::Response {
            content_disposition: Some(content_disposition_for(
                remote_response.content.content_type.as_deref(),
                Some(body.filename.clone()),
            )),
            content_type: remote_response.content.content_type,
            file: remote_response.content.file,
        })
    } else {
        Err(Error::BadRequest(ErrorKind::NotFound, "Media not found."))
    }
}

fn fix_thumbnail_headers(r: &mut axum::response::Response) {
    let content_type = r
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|x| std::str::from_utf8(x.as_ref()).ok())
        .map(ToOwned::to_owned);

    set_header_or_panic(r, CONTENT_SECURITY_POLICY, content_security_policy());
    set_header_or_panic(
        r,
        CONTENT_DISPOSITION,
        content_disposition_for(content_type.as_deref(), None)
            .to_string()
            .try_into()
            .expect("generated header value should be valid"),
    );
}

/// # `GET /_matrix/media/r0/thumbnail/{serverName}/{mediaId}`
///
/// Load media thumbnail from our server or over federation.
///
/// - Only allows federation if `allow_remote` is true
#[allow(deprecated)] // unauthenticated media
pub(crate) async fn get_content_thumbnail_legacy_route(
    body: Ar<legacy_media::get_content_thumbnail::v3::Request>,
) -> Result<axum::response::Response> {
    use authenticated_media_client::get_content_thumbnail::v1::{
        Request as AmRequest, Response as AmResponse,
    };
    use legacy_media::get_content_thumbnail::v3::{
        Request as LegacyRequest, Response as LegacyResponse,
    };

    fn convert_request(
        LegacyRequest {
            server_name,
            media_id,
            method,
            width,
            height,
            timeout_ms,
            animated,
            ..
        }: LegacyRequest,
    ) -> AmRequest {
        AmRequest {
            server_name,
            media_id,
            method,
            width,
            height,
            timeout_ms,
            animated,
        }
    }

    fn convert_response(
        AmResponse {
            file,
            content_type,
        }: AmResponse,
    ) -> LegacyResponse {
        LegacyResponse {
            file,
            content_type,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        }
    }

    let allow_remote = body.allow_remote;

    get_content_thumbnail_route_ruma(
        body.map_body(convert_request),
        allow_remote,
    )
    .await
    .map(|response| {
        let response = convert_response(response);
        let mut r = Ra(response).into_response();

        fix_thumbnail_headers(&mut r);

        r
    })
}

/// # `GET /_matrix/client/v1/media/thumbnail/{serverName}/{mediaId}`
///
/// Load media thumbnail from our server or over federation.
pub(crate) async fn get_content_thumbnail_route(
    body: Ar<authenticated_media_client::get_content_thumbnail::v1::Request>,
) -> Result<axum::response::Response> {
    get_content_thumbnail_route_ruma(body, true).await.map(|x| {
        let mut r = Ra(x).into_response();

        fix_thumbnail_headers(&mut r);

        r
    })
}

#[tracing::instrument(skip_all)]
async fn get_remote_thumbnail_via_federation_api(
    server_name: &ruma::ServerName,
    request: authenticated_media_fed::get_content_thumbnail::v1::Request,
) -> Result<RemoteResponse, Error> {
    let authenticated_media_fed::get_content_thumbnail::v1::Response {
        metadata,
        content,
    } = services()
        .sending
        .send_federation_request(server_name, request)
        .await?;

    let content = match content {
        authenticated_media_fed::FileOrLocation::File(content) => {
            debug!("Got thumbnail from remote server");
            content
        }
        authenticated_media_fed::FileOrLocation::Location(location) => {
            debug!(location, "Following redirect");
            get_redirected_content(location).await?
        }
    };

    Ok(RemoteResponse {
        metadata,
        content,
    })
}

#[allow(deprecated)] // unauthenticated media
#[tracing::instrument(skip_all)]
async fn get_remote_thumbnail_via_legacy_api(
    server_name: &ruma::ServerName,
    authenticated_media_fed::get_content_thumbnail::v1::Request {
        media_id,
        method,
        width,
        height,
        timeout_ms,
        animated,
    }: authenticated_media_fed::get_content_thumbnail::v1::Request,
) -> Result<RemoteResponse, Error> {
    let content_response = services()
        .sending
        .send_federation_request(
            server_name,
            legacy_media::get_content_thumbnail::v3::Request {
                server_name: server_name.to_owned(),
                allow_remote: false,
                allow_redirect: false,
                media_id,
                method,
                width,
                height,
                timeout_ms,
                animated,
            },
        )
        .await?;

    Ok(RemoteResponse {
        metadata: authenticated_media_fed::ContentMetadata {},
        content: authenticated_media_fed::Content {
            file: content_response.file,
            content_disposition: None,
            content_type: content_response.content_type,
        },
    })
}

#[tracing::instrument]
pub(crate) async fn get_remote_thumbnail(
    server_name: &ruma::ServerName,
    request: authenticated_media_fed::get_content_thumbnail::v1::Request,
) -> Result<RemoteResponse, Error> {
    let fed_result =
        get_remote_thumbnail_via_federation_api(server_name, request.clone())
            .await;

    let response = match fed_result {
        Ok(response) => {
            debug!("Got remote content via authenticated media API");
            response
        }
        Err(Error::Federation(_, error))
            if error.error_kind() == Some(&ErrorKind::Unrecognized) =>
        {
            info!(
                "Remote server does not support authenticated media, falling \
                 back to deprecated API"
            );

            get_remote_thumbnail_via_legacy_api(server_name, request.clone())
                .await?
        }
        Err(e) => {
            return Err(e);
        }
    };

    Ok(response)
}

async fn get_content_thumbnail_route_ruma(
    body: Ar<authenticated_media_client::get_content_thumbnail::v1::Request>,
    allow_remote: bool,
) -> Result<authenticated_media_client::get_content_thumbnail::v1::Response> {
    let mxc = MxcData::new(&body.server_name, &body.media_id)?;
    let width = body.width.try_into().map_err(|_| {
        Error::BadRequest(ErrorKind::InvalidParam, "Width is invalid.")
    })?;
    let height = body.height.try_into().map_err(|_| {
        Error::BadRequest(ErrorKind::InvalidParam, "Height is invalid.")
    })?;

    let make_response = |file, content_type| {
        authenticated_media_client::get_content_thumbnail::v1::Response {
            file,
            content_type,
        }
    };

    if let Some((
        FileMeta {
            content_type,
            ..
        },
        file,
    )) =
        services().media.get_thumbnail(mxc.to_string(), width, height).await?
    {
        return Ok(make_response(file, content_type));
    }

    if &*body.server_name != services().globals.server_name() && allow_remote {
        let get_thumbnail_response = get_remote_thumbnail(
            &body.server_name,
            authenticated_media_fed::get_content_thumbnail::v1::Request {
                height: body.height,
                width: body.width,
                method: body.method.clone(),
                media_id: body.media_id.clone(),
                timeout_ms: Duration::from_secs(20),
                // we don't support animated thumbnails, so don't try requesting
                // one - we're allowed to ignore the client's request for an
                // animated thumbnail
                animated: Some(false),
            },
        )
        .await;

        match get_thumbnail_response {
            Ok(resp) => {
                services()
                    .media
                    .upload_thumbnail(
                        mxc.to_string(),
                        None,
                        resp.content.content_type.as_deref(),
                        width,
                        height,
                        &resp.content.file,
                    )
                    .await?;

                return Ok(make_response(
                    resp.content.file,
                    resp.content.content_type,
                ));
            }
            Err(error) => warn!(
                %error,
                "Failed to fetch thumbnail via federation, trying to fetch \
                 original media and create thumbnail ourselves"
            ),
        }

        get_remote_content(&mxc).await?;

        if let Some((
            FileMeta {
                content_type,
                ..
            },
            file,
        )) = services()
            .media
            .get_thumbnail(mxc.to_string(), width, height)
            .await?
        {
            return Ok(make_response(file, content_type));
        }

        error!("Source media doesn't exist even after fetching it from remote");
    }

    Err(Error::BadRequest(ErrorKind::NotYetUploaded, "Media not found."))
}
