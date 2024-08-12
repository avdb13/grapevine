use std::time::Duration;

use axum::response::IntoResponse;
use http::{
    header::{CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE},
    HeaderName, HeaderValue,
};
use phf::{phf_set, Set};
use ruma::api::client::{
    error::ErrorKind,
    media::{
        create_content, get_content, get_content_as_filename,
        get_content_thumbnail, get_media_config,
    },
};
use tracing::error;

use crate::{service::media::FileMeta, services, utils, Ar, Error, Ra, Result};

const MXC_LENGTH: usize = 32;

/// `Content-Type`s that can be rendered inline in a browser without risking XSS
///
/// Cargo-culted from Synapse. Note that SVG can contain inline JavaScript.
pub(crate) static INLINE_CONTENT_TYPES: Set<&str> = phf_set! {
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
    filename: Option<&str>,
) -> String {
    match (
        content_type.is_some_and(|x| INLINE_CONTENT_TYPES.contains(x)),
        filename,
    ) {
        (true, None) => "inline".to_owned(),
        (true, Some(x)) => format!("inline; filename={x}"),
        (false, None) => "attachment".to_owned(),
        (false, Some(x)) => format!("attachment; filename={x}"),
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
pub(crate) async fn get_media_config_route(
    _body: Ar<get_media_config::v3::Request>,
) -> Result<Ra<get_media_config::v3::Response>> {
    Ok(Ra(get_media_config::v3::Response {
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
    let mxc = format!(
        "mxc://{}/{}",
        services().globals.server_name(),
        utils::random_string(MXC_LENGTH)
    );

    services()
        .media
        .create(
            mxc.clone(),
            body.filename
                .as_ref()
                .map(|filename| format!("inline; filename={filename}"))
                .as_deref(),
            body.content_type.as_deref(),
            &body.file,
        )
        .await?;

    Ok(Ra(create_content::v3::Response {
        content_uri: mxc.into(),
        blurhash: None,
    }))
}

pub(crate) async fn get_remote_content(
    mxc: &str,
    server_name: &ruma::ServerName,
    media_id: String,
) -> Result<get_content::v3::Response, Error> {
    let content_response = services()
        .sending
        .send_federation_request(
            server_name,
            get_content::v3::Request {
                allow_remote: false,
                server_name: server_name.to_owned(),
                media_id,
                timeout_ms: Duration::from_secs(20),
                allow_redirect: false,
            },
        )
        .await?;

    services()
        .media
        .create(
            mxc.to_owned(),
            content_response.content_disposition.as_deref(),
            content_response.content_type.as_deref(),
            &content_response.file,
        )
        .await?;

    Ok(get_content::v3::Response {
        file: content_response.file,
        content_disposition: content_response.content_disposition,
        content_type: content_response.content_type,
        cross_origin_resource_policy: Some("cross-origin".to_owned()),
    })
}

/// # `GET /_matrix/media/r0/download/{serverName}/{mediaId}`
///
/// Load media from our server or over federation.
///
/// - Only allows federation if `allow_remote` is true
pub(crate) async fn get_content_route(
    body: Ar<get_content::v3::Request>,
) -> Result<axum::response::Response> {
    get_content_route_ruma(body).await.map(|x| {
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
    body: Ar<get_content::v3::Request>,
) -> Result<get_content::v3::Response> {
    let mxc = format!("mxc://{}/{}", body.server_name, body.media_id);

    if let Some(FileMeta {
        content_type,
        file,
        ..
    }) = services().media.get(mxc.clone()).await?
    {
        Ok(get_content::v3::Response {
            file,
            content_disposition: Some(content_disposition_for(
                content_type.as_deref(),
                None,
            )),
            content_type,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        })
    } else if &*body.server_name != services().globals.server_name()
        && body.allow_remote
    {
        let remote_content_response =
            get_remote_content(&mxc, &body.server_name, body.media_id.clone())
                .await?;
        Ok(get_content::v3::Response {
            file: remote_content_response.file,
            content_disposition: Some(content_disposition_for(
                remote_content_response.content_type.as_deref(),
                None,
            )),
            content_type: remote_content_response.content_type,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
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
pub(crate) async fn get_content_as_filename_route(
    body: Ar<get_content_as_filename::v3::Request>,
) -> Result<axum::response::Response> {
    get_content_as_filename_route_ruma(body).await.map(|x| {
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
    body: Ar<get_content_as_filename::v3::Request>,
) -> Result<get_content_as_filename::v3::Response> {
    let mxc = format!("mxc://{}/{}", body.server_name, body.media_id);

    if let Some(FileMeta {
        content_type,
        file,
        ..
    }) = services().media.get(mxc.clone()).await?
    {
        Ok(get_content_as_filename::v3::Response {
            file,
            content_disposition: Some(content_disposition_for(
                content_type.as_deref(),
                Some(body.filename.as_str()),
            )),
            content_type,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        })
    } else if &*body.server_name != services().globals.server_name()
        && body.allow_remote
    {
        let remote_content_response =
            get_remote_content(&mxc, &body.server_name, body.media_id.clone())
                .await?;

        Ok(get_content_as_filename::v3::Response {
            content_disposition: Some(content_disposition_for(
                remote_content_response.content_type.as_deref(),
                Some(body.filename.as_str()),
            )),
            content_type: remote_content_response.content_type,
            file: remote_content_response.file,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        })
    } else {
        Err(Error::BadRequest(ErrorKind::NotFound, "Media not found."))
    }
}

/// # `GET /_matrix/media/r0/thumbnail/{serverName}/{mediaId}`
///
/// Load media thumbnail from our server or over federation.
///
/// - Only allows federation if `allow_remote` is true
pub(crate) async fn get_content_thumbnail_route(
    body: Ar<get_content_thumbnail::v3::Request>,
) -> Result<axum::response::Response> {
    get_content_thumbnail_route_ruma(body).await.map(|x| {
        let mut r = Ra(x).into_response();

        let content_type = r
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|x| std::str::from_utf8(x.as_ref()).ok())
            .map(ToOwned::to_owned);

        set_header_or_panic(
            &mut r,
            CONTENT_SECURITY_POLICY,
            content_security_policy(),
        );
        set_header_or_panic(
            &mut r,
            CONTENT_DISPOSITION,
            content_disposition_for(content_type.as_deref(), None)
                .try_into()
                .expect("generated header value should be valid"),
        );

        r
    })
}

async fn get_content_thumbnail_route_ruma(
    body: Ar<get_content_thumbnail::v3::Request>,
) -> Result<get_content_thumbnail::v3::Response> {
    let mxc = format!("mxc://{}/{}", body.server_name, body.media_id);

    if let Some(FileMeta {
        content_type,
        file,
        ..
    }) = services()
        .media
        .get_thumbnail(
            mxc.clone(),
            body.width.try_into().map_err(|_| {
                Error::BadRequest(ErrorKind::InvalidParam, "Width is invalid.")
            })?,
            body.height.try_into().map_err(|_| {
                Error::BadRequest(ErrorKind::InvalidParam, "Width is invalid.")
            })?,
        )
        .await?
    {
        Ok(get_content_thumbnail::v3::Response {
            file,
            content_type,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        })
    } else if &*body.server_name != services().globals.server_name()
        && body.allow_remote
    {
        let get_thumbnail_response = services()
            .sending
            .send_federation_request(
                &body.server_name,
                get_content_thumbnail::v3::Request {
                    allow_remote: false,
                    height: body.height,
                    width: body.width,
                    method: body.method.clone(),
                    server_name: body.server_name.clone(),
                    media_id: body.media_id.clone(),
                    timeout_ms: Duration::from_secs(20),
                    allow_redirect: false,
                },
            )
            .await?;

        services()
            .media
            .upload_thumbnail(
                mxc,
                None,
                get_thumbnail_response.content_type.as_deref(),
                body.width.try_into().expect("all UInts are valid u32s"),
                body.height.try_into().expect("all UInts are valid u32s"),
                &get_thumbnail_response.file,
            )
            .await?;

        Ok(get_content_thumbnail::v3::Response {
            file: get_thumbnail_response.file,
            content_type: get_thumbnail_response.content_type,
            cross_origin_resource_policy: Some("cross-origin".to_owned()),
        })
    } else {
        Err(Error::BadRequest(ErrorKind::NotYetUploaded, "Media not found."))
    }
}
