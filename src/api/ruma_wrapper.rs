use std::ops::Deref;

use ruma::{
    api::client::uiaa::UiaaResponse, CanonicalJsonValue, OwnedDeviceId,
    OwnedServerName, OwnedUserId,
};

use crate::{service::appservice::RegistrationInfo, Error};

mod axum;

/// A wrapper to convert an Axum request to Ruma data
///
/// Named so because this converts from **A**xum to **R**uma. See also [`Ra`],
/// which is roughly the inverse of this type.
pub(crate) struct Ar<T> {
    /// The Ruma type to deserialize the body into
    pub(crate) body: T,
    pub(crate) sender_user: Option<OwnedUserId>,
    pub(crate) sender_device: Option<OwnedDeviceId>,
    pub(crate) sender_servername: Option<OwnedServerName>,
    // This is None when body is not a valid string
    pub(crate) json_body: Option<CanonicalJsonValue>,
    pub(crate) appservice_info: Option<RegistrationInfo>,
}

impl<T> Ar<T> {
    pub(crate) fn map_body<F, U>(self, f: F) -> Ar<U>
    where
        F: FnOnce(T) -> U,
    {
        let Ar {
            body,
            sender_user,
            sender_device,
            sender_servername,
            json_body,
            appservice_info,
        } = self;

        Ar {
            body: f(body),
            sender_user,
            sender_device,
            sender_servername,
            json_body,
            appservice_info,
        }
    }
}

impl<T> Deref for Ar<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.body
    }
}

/// A wrapper to convert Ruma data to an Axum response
///
/// Named so because this converts from **R**uma to **A**xum. See also [`Ar`],
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
