mod data;
use std::{fmt::Debug, mem};

use bytes::BytesMut;
pub(crate) use data::Data;
use ruma::{
    api::{
        client::push::{set_pusher, Pusher, PusherKind},
        push_gateway::send_event_notification::{
            self,
            v1::{
                Device, Notification, NotificationCounts, NotificationPriority,
            },
        },
        IncomingResponse, MatrixVersion, OutgoingRequest, SendAccessToken,
    },
    events::{
        room::power_levels::RoomPowerLevelsEventContent, AnySyncTimelineEvent,
        StateEventType, TimelineEventType,
    },
    push::{
        Action, PushConditionPowerLevelsCtx, PushConditionRoomCtx, PushFormat,
        Ruleset, Tweak,
    },
    serde::Raw,
    uint, RoomId, UInt, UserId,
};
use tracing::warn;

use crate::{services, utils, Error, PduEvent, Result};

pub(crate) struct Service {
    pub(crate) db: &'static dyn Data,
}

impl Service {
    pub(crate) fn set_pusher(
        &self,
        sender: &UserId,
        pusher: set_pusher::v3::PusherAction,
    ) -> Result<()> {
        self.db.set_pusher(sender, pusher)
    }

    pub(crate) fn get_pusher(
        &self,
        sender: &UserId,
        pushkey: &str,
    ) -> Result<Option<Pusher>> {
        self.db.get_pusher(sender, pushkey)
    }

    pub(crate) fn get_pushers(&self, sender: &UserId) -> Result<Vec<Pusher>> {
        self.db.get_pushers(sender)
    }

    pub(crate) fn get_pushkeys(
        &self,
        sender: &UserId,
    ) -> Box<dyn Iterator<Item = Result<String>>> {
        self.db.get_pushkeys(sender)
    }

    #[tracing::instrument(skip(self, destination, request))]
    pub(crate) async fn send_request<T>(
        &self,
        destination: &str,
        request: T,
    ) -> Result<T::IncomingResponse>
    where
        T: OutgoingRequest + Debug,
    {
        let destination = destination.replace("/_matrix/push/v1/notify", "");

        let http_request = request
            .try_into_http_request::<BytesMut>(
                &destination,
                SendAccessToken::IfRequired(""),
                &[MatrixVersion::V1_0],
            )
            .map_err(|error| {
                warn!(%error, %destination, "Failed to find destination");
                Error::BadServerResponse("Invalid destination")
            })?
            .map(BytesMut::freeze);

        let reqwest_request = reqwest::Request::try_from(http_request)?;

        let url = reqwest_request.url().clone();
        let response =
            services().globals.default_client().execute(reqwest_request).await;

        match response {
            Ok(mut response) => {
                // reqwest::Response -> http::Response conversion
                let status = response.status();
                let mut http_response_builder = http::Response::builder()
                    .status(status)
                    .version(response.version());
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
                        push_gateway = %destination,
                        %status,
                        %url,
                        body = %utils::dbg_truncate_str(
                            String::from_utf8_lossy(&body).as_ref(),
                            100,
                        ),
                        "Push gateway returned bad response",
                    );
                }

                let response = T::IncomingResponse::try_from_http_response(
                    http_response_builder
                        .body(body)
                        .expect("reqwest body is valid http body"),
                );
                response.map_err(|error| {
                    warn!(
                        %error,
                        appservice = %destination,
                        %url,
                        "Push gateway returned invalid response bytes",
                    );
                    Error::BadServerResponse(
                        "Push gateway returned bad response.",
                    )
                })
            }
            Err(error) => {
                warn!(
                    %error,
                    %destination,
                    "Could not send request to push gateway",
                );
                Err(error.into())
            }
        }
    }

    #[tracing::instrument(skip(self, user, unread, pusher, ruleset, pdu))]
    pub(crate) async fn send_push_notice(
        &self,
        user: &UserId,
        unread: UInt,
        pusher: &Pusher,
        ruleset: Ruleset,
        pdu: &PduEvent,
    ) -> Result<()> {
        let mut notify = None;
        let mut tweaks = Vec::new();

        let power_levels: RoomPowerLevelsEventContent = services()
            .rooms
            .state_accessor
            .room_state_get(&pdu.room_id, &StateEventType::RoomPowerLevels, "")?
            .map(|ev| {
                serde_json::from_str(ev.content.get()).map_err(|_| {
                    Error::bad_database("invalid m.room.power_levels event")
                })
            })
            .transpose()?
            .unwrap_or_default();

        for action in self.get_actions(
            user,
            &ruleset,
            &power_levels,
            &pdu.to_sync_room_event(),
            &pdu.room_id,
        )? {
            let n = match action {
                Action::Notify => true,
                Action::SetTweak(tweak) => {
                    tweaks.push(tweak.clone());
                    continue;
                }
                _ => false,
            };

            if notify.is_some() {
                return Err(Error::bad_database(
                    r#"Malformed pushrule contains more than one of these actions: ["dont_notify", "notify", "coalesce"]"#,
                ));
            }

            notify = Some(n);
        }

        if notify == Some(true) {
            self.send_notice(unread, pusher, tweaks, pdu).await?;
        }
        // Else the event triggered no actions

        Ok(())
    }

    #[tracing::instrument(skip(self, user, ruleset, pdu))]
    pub(crate) fn get_actions<'a>(
        &self,
        user: &UserId,
        ruleset: &'a Ruleset,
        power_levels: &RoomPowerLevelsEventContent,
        pdu: &Raw<AnySyncTimelineEvent>,
        room_id: &RoomId,
    ) -> Result<&'a [Action]> {
        let power_levels = PushConditionPowerLevelsCtx {
            users: power_levels.users.clone(),
            users_default: power_levels.users_default,
            notifications: power_levels.notifications.clone(),
        };

        let ctx = PushConditionRoomCtx {
            room_id: room_id.to_owned(),
            // TODO: get member count efficiently
            member_count: 10_u32.into(),
            user_id: user.to_owned(),
            user_display_name: services()
                .users
                .displayname(user)?
                .unwrap_or_else(|| user.localpart().to_owned()),
            power_levels: Some(power_levels),
        };

        Ok(ruleset.get_actions(pdu, &ctx))
    }

    #[tracing::instrument(skip(self, unread, pusher, tweaks, event))]
    async fn send_notice(
        &self,
        unread: UInt,
        pusher: &Pusher,
        tweaks: Vec<Tweak>,
        event: &PduEvent,
    ) -> Result<()> {
        // TODO: email
        match &pusher.kind {
            PusherKind::Http(http) => {
                // TODO:
                // Two problems with this
                // 1. if "event_id_only" is the only format kind it seems we
                //    should never add more info
                // 2. can pusher/devices have conflicting formats
                let event_id_only =
                    http.format == Some(PushFormat::EventIdOnly);

                let mut device = Device::new(
                    pusher.ids.app_id.clone(),
                    pusher.ids.pushkey.clone(),
                );
                device.data.default_payload = http.default_payload.clone();
                device.data.format.clone_from(&http.format);

                // Tweaks are only added if the format is NOT event_id_only
                if !event_id_only {
                    device.tweaks.clone_from(&tweaks);
                }

                let d = vec![device];
                let mut notifi = Notification::new(d);

                notifi.prio = NotificationPriority::Low;
                notifi.event_id = Some((*event.event_id).to_owned());
                notifi.room_id = Some((*event.room_id).to_owned());
                // TODO: missed calls
                notifi.counts = NotificationCounts::new(unread, uint!(0));

                if event.kind == TimelineEventType::RoomEncrypted
                    || tweaks.iter().any(|t| {
                        matches!(t, Tweak::Highlight(true) | Tweak::Sound(_))
                    })
                {
                    notifi.prio = NotificationPriority::High;
                }

                if event_id_only {
                    self.send_request(
                        &http.url,
                        send_event_notification::v1::Request::new(notifi),
                    )
                    .await?;
                } else {
                    notifi.sender = Some(event.sender.clone());
                    notifi.event_type = Some(event.kind.clone());
                    notifi.content =
                        serde_json::value::to_raw_value(&event.content).ok();

                    if event.kind == TimelineEventType::RoomMember {
                        notifi.user_is_target = event.state_key.as_deref()
                            == Some(event.sender.as_str());
                    }

                    notifi.sender_display_name =
                        services().users.displayname(&event.sender)?;

                    notifi.room_name = services()
                        .rooms
                        .state_accessor
                        .get_name(&event.room_id)?;

                    self.send_request(
                        &http.url,
                        send_event_notification::v1::Request::new(notifi),
                    )
                    .await?;
                }

                Ok(())
            }
            // TODO: Handle email
            _ => Ok(()),
        }
    }
}
