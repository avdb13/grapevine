mod data;

use std::sync::Arc;

pub(crate) use data::Data;
use http::{header::USER_AGENT, HeaderValue};
use mas_http::{
    BodyToBytesResponseLayer, BytesToBodyRequestLayer, HttpService,
};
use mas_oidc_client::{
    requests::discovery, types::oidc::VerifiedProviderMetadata,
};
use tower::{buffer::Buffer, ServiceBuilder, ServiceExt};
use tower_http::ServiceBuilderExt;
use tracing::error;

use crate::{config::ProviderConfig, services, Error, Result};

pub(crate) struct Service {
    pub(crate) db: &'static dyn Data,
    pub(crate) client: HttpService,
}

impl Service {
    pub(crate) fn build(db: &'static dyn Data) -> Arc<Self> {
        let user_agent = HeaderValue::from_str(
            &[env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")].join("/"),
        )
        .expect("user agent should be valid");

        let service = ServiceBuilder::new()
            .layer(BytesToBodyRequestLayer)
            .layer(BodyToBytesResponseLayer);

        Arc::new(Self {
            db,
            client: HttpService::new(Buffer::new(
                service
                    .override_request_header(USER_AGENT, user_agent)
                    .service(mas_http::make_untraced_client())
                    .boxed(),
                16,
            )),
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Provider {
    pub(crate) config: &'static ProviderConfig,
    pub(crate) metadata: VerifiedProviderMetadata,
}

impl Provider {
    pub(crate) async fn fetch_metadata(
        config: &'static ProviderConfig,
    ) -> Result<Self> {
        match discovery::discover(&services().sso.client, &config.issuer).await
        {
            Ok(metadata) => Ok(Provider {
                config,
                metadata,
            }),
            Err(error) => {
                error!(
                    %error,
                    provider = %config.inner.id,
                    "Failed to fetch identity provider metadata",
                );

                Err(Error::bad_config(
                    "Failed to fetch identity provider metadata. Please check \
                     your config.",
                ))
            }
        }
    }
}

impl PartialEq for Provider {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
    }
}

impl Eq for Provider {}
