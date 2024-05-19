use std::ops::Deref;

use ruma::{
    api::client::uiaa::UiaaResponse, CanonicalJsonValue, OwnedDeviceId,
    OwnedServerName, OwnedUserId,
};

use crate::{service::appservice::RegistrationInfo, Error};

mod axum;

/// Extractor for Ruma request structs
pub(crate) struct Ruma<T> {
    pub(crate) body: T,
    pub(crate) sender_user: Option<OwnedUserId>,
    pub(crate) sender_device: Option<OwnedDeviceId>,
    pub(crate) sender_servername: Option<OwnedServerName>,
    // This is None when body is not a valid string
    pub(crate) json_body: Option<CanonicalJsonValue>,
    pub(crate) appservice_info: Option<RegistrationInfo>,
}

impl<T> Deref for Ruma<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.body
    }
}

/// A wrapper to convert Ruma data to an Axum response
///
/// Named so because this converts from **R**uma to **A**xum. See also [`Ruma`],
/// which is roughly the inverse of this type.
#[derive(Clone)]
pub(crate) struct Ra<T>(pub(crate) T);

impl<T> From<T> for Ra<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

impl From<Error> for Ra<UiaaResponse> {
    fn from(t: Error) -> Self {
        t.to_response()
    }
}
