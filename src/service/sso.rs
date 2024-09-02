mod data;

use std::sync::Arc;

pub(crate) use data::Data;
use http::{header::USER_AGENT, HeaderValue};
use mas_http::{
    BodyToBytesResponseLayer, BytesToBodyRequestLayer, HttpService,
};
use tower::{buffer::Buffer, ServiceBuilder, ServiceExt};
use tower_http::ServiceBuilderExt;

pub(crate) struct Service {
    pub(crate) db: &'static dyn Data,
    pub(crate) http_service: HttpService,
}

impl Service {
    pub(crate) fn build(db: &'static dyn Data) -> Arc<Self> {
        let user_agent = HeaderValue::from_str(
            &[env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")].join("/"),
        )
        .expect("user agent should be valid");

        let service = ServiceBuilder::new()
            .layer(BytesToBodyRequestLayer)
            .layer(BodyToBytesResponseLayer)
            .override_request_header(USER_AGENT, user_agent)
            .service(mas_http::make_untraced_client());

        Arc::new(Self {
            db,
            http_service: HttpService::new(Buffer::new(service.boxed(), 16)),
        })
    }
}
