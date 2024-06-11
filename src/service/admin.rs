use std::{collections::BTreeMap, convert::TryInto, sync::Arc};

use regex::Regex;
use ruma::{
    events::{
        room::{
            canonical_alias::RoomCanonicalAliasEventContent,
            create::RoomCreateEventContent,
            guest_access::{GuestAccess, RoomGuestAccessEventContent},
            history_visibility::{
                HistoryVisibility, RoomHistoryVisibilityEventContent,
            },
            join_rules::{JoinRule, RoomJoinRulesEventContent},
            member::{MembershipState, RoomMemberEventContent},
            message::RoomMessageEventContent,
            name::RoomNameEventContent,
            power_levels::RoomPowerLevelsEventContent,
            topic::RoomTopicEventContent,
        },
        TimelineEventType,
    },
    OwnedRoomAliasId, OwnedRoomId, OwnedUserId, RoomAliasId, RoomId,
    RoomVersionId, UserId,
};
use serde_json::value::to_raw_value;
use tokio::sync::{mpsc, Mutex};

use super::pdu::PduBuilder;
use crate::{services, utils::truncate_str_for_debug, Result};

mod clear_service_caches;
mod common;
mod create_user;
mod deactivate_all;
mod deactivate_user;
mod disable_room;
mod enable_room;
mod get_auth_chain;
mod get_pdu;
mod incoming_federation;
mod list_appservices;
mod list_local_users;
mod list_rooms;
mod memory_usage;
mod parse_pdu;
mod register_appservice;
mod reset_password;
mod show_config;
mod sign_json;
mod unregister_appservice;
mod verify_json;

#[derive(Debug)]
pub(crate) enum AdminRoomEvent {
    ProcessMessage(String),
    SendMessage(RoomMessageEventContent),
}

pub(crate) struct Service {
    pub(crate) sender: mpsc::UnboundedSender<AdminRoomEvent>,
    receiver: Mutex<mpsc::UnboundedReceiver<AdminRoomEvent>>,
}

#[derive(Debug, Clone, ValueEnum)]
enum TracingBackend {
    Log,
    Flame,
    Traces,
}

fn get_grapevine_user() -> OwnedUserId {
    UserId::parse(format!(
        "@{}:{}",
        if services().globals.config.conduit_compat {
            "conduit"
        } else {
            "grapevine"
        },
        services().globals.server_name()
    ))
    .expect("Admin bot username should always be valid")
}

impl Service {
    pub(crate) fn build() -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            sender,
            receiver: Mutex::new(receiver),
        })
    }

    pub(crate) fn start_handler(self: &Arc<Self>) {
        let self2 = Arc::clone(self);
        tokio::spawn(async move {
            let mut receiver = self2.receiver.lock().await;
            let grapevine_user = get_grapevine_user();

            let Ok(Some(grapevine_room)) = services().admin.get_admin_room()
            else {
                return;
            };

            loop {
                let event = receiver
                    .recv()
                    .await
                    .expect("admin command channel has been closed");

                Self::handle_event(&self2, event, &grapevine_room).await;
            }
        });
    }

    #[tracing::instrument(skip(self, grapevine_room, grapevine_user))]
    async fn handle_event(
        &self,
        event: AdminRoomEvent,
        grapevine_room: &OwnedRoomId,
        grapevine_user: &OwnedUserId,
    ) {
        let message_content = match event {
            AdminRoomEvent::SendMessage(content) => content,
            AdminRoomEvent::ProcessMessage(room_message) => {
                self.process_admin_message(room_message).await
            }
        };

        let room_token = services()
            .globals
            .roomid_mutex_state
            .lock_key(grapevine_room.clone())
            .await;

        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomMessage,
                    content: to_raw_value(&message_content)
                        .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: None,
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await
            .unwrap();
    }

    #[tracing::instrument(
        skip(self, room_message),
        fields(
            room_message = dbg_truncate_str(&room_message, 50).as_ref(),
        ),
    )]
    pub(crate) fn process_message(&self, room_message: String) {
        self.sender.send(AdminRoomEvent::ProcessMessage(room_message)).unwrap();
    }

    #[tracing::instrument(skip(self, message_content))]
    pub(crate) fn send_message(
        &self,
        message_content: RoomMessageEventContent,
    ) {
        self.sender.send(AdminRoomEvent::SendMessage(message_content)).unwrap();
    }

    // Parse and process a message from the admin room
    #[tracing::instrument(
        skip(self, room_message),
        fields(
            room_message = dbg_truncate_str(&room_message, 50).as_ref(),
        ),
    )]
    async fn process_admin_message(
        &self,
        room_message: String,
    ) -> RoomMessageEventContent {
        let mut lines = room_message.lines().filter(|l| !l.trim().is_empty());
        let command_line =
            lines.next().expect("each string has at least one line");
        let body: Vec<_> = lines.collect();

        let mut argv: Vec<_> = command_line.split_whitespace().collect();

        // Replace `help command` with `command --help`
        // Clap has a help subcommand, but it omits the long help description.
        if argv.len() > 1 && argv[1] == "help" {
            argv.remove(1);
            argv.push("--help");
        }

        let outcome = match argv.get(1) {
            Some(&"clear-service-caches") => {
                clear_service_caches::try_process(argv).await
            }
            Some(&"create-user") => create_user::try_process(argv),
            Some(&"deactivate-all") => {
                deactivate_all::try_process(argv, body).await
            }
            Some(&"deactivate-user") => {
                deactivate_user::try_process(argv).await
            }
            Some(&"disable-room") => disable_room::try_process(argv),
            Some(&"enable-room") => enable_room::try_process(argv),
            Some(&"get-auth-chain") => get_auth_chain::try_process(argv).await,
            Some(&"get-pdu") => get_pdu::try_process(argv),
            Some(&"incoming-federation") => {
                incoming_federation::try_process().await
            }
            Some(&"list-appservices") => list_appservices::try_process().await,
            Some(&"list-local-users") => list_local_users::try_process(),
            Some(&"list-rooms") => list_rooms::try_process(),
            Some(&"memory-usage") => memory_usage::try_process().await,
            Some(&"parse-pdu") => parse_pdu::try_process(&body),
            Some(&"register-appservice") => {
                register_appservice::try_process(body).await
            }
            Some(&"reset-password") => reset_password::try_process(argv),
            Some(&"show-config") => show_config::try_process(),
            Some(&"sign-json") => sign_json::try_process(body),
            Some(&"unregister-appservice") => {
                unregister_appservice::try_process(argv).await
            }
            Some(&"verify-json") => verify_json::try_process(body).await,
            Some(_) => Err("Command not recognized".to_owned()),
            None => Err("No command provided".to_owned()),
        };

        match outcome {
            Ok(reply_message) => {
                RoomMessageEventContent::text_plain(reply_message)
            }
            Err(e) => {
                let markdown_message = format!(
                    "Encountered an error while handling the \
                     command:\n```{e}```"
                );
                let html_message = format!(
                    "Encountered an error while handling the \
                     command:\n<pre>\n{e}\n</pre>"
                );
                RoomMessageEventContent::text_html(
                    markdown_message,
                    html_message,
                )
            }
        }
    }

    // Utility to turn clap's `--help` text to HTML.
    #[tracing::instrument(skip_all)]
    fn usage_to_html(text: &str, server_name: &ServerName) -> String {
        // Replace `@grapevine:servername:-subcmdname` with
        // `@grapevine:servername: subcmdname`
        let localpart = if services().globals.config.conduit_compat {
            "conduit"
        } else {
            "grapevine"
        };

        let text = text.replace(
            &format!("@{localpart}:{server_name}:-"),
            &format!("@{localpart}:{server_name}: "),
        );

        // For the grapevine admin room, subcommands become main commands
        let text = text.replace("SUBCOMMAND", "COMMAND");
        let text = text.replace("subcommand", "command");

        // Escape option names (e.g. `<element-id>`) since they look like HTML
        // tags
        let text = text.replace('<', "&lt;").replace('>', "&gt;");

        // Italicize the first line (command name and version text)
        let re =
            Regex::new("^(.*?)\n").expect("Regex compilation should not fail");
        let text = re.replace_all(&text, "<em>$1</em>\n");

        // Unmerge wrapped lines
        let text = text.replace("\n            ", "  ");

        // Wrap option names in backticks. The lines look like:
        //     -V, --version  Prints version information
        // And are converted to:
        // <code>-V, --version</code>: Prints version information
        // (?m) enables multi-line mode for ^ and $
        let re = Regex::new("(?m)^ {4}(([a-zA-Z_&;-]+(, )?)+)  +(.*)$")
            .expect("Regex compilation should not fail");
        let text = re.replace_all(&text, "<code>$1</code>: $4");

        // Look for a `[commandbody]()` tag. If it exists, use all lines below
        // it that start with a `#` in the USAGE section.
        let mut text_lines: Vec<&str> = text.lines().collect();
        let command_body = text_lines
            .iter()
            .skip_while(|x| x != &&"[commandbody]()")
            .skip(1)
            .map_while(|&x| x.strip_prefix('#'))
            .map(|x| x.strip_prefix(' ').unwrap_or(x))
            .collect::<String>();

        text_lines.retain(|x| x != &"[commandbody]()");
        let text = text_lines.join("\n");

        // Improve the usage section
        let text = if command_body.is_empty() {
            // Wrap the usage line in code tags
            let re = Regex::new("(?m)^USAGE:\n {4}(@grapevine:.*)$")
                .expect("Regex compilation should not fail");
            re.replace_all(&text, "USAGE:\n<code>$1</code>").to_string()
        } else {
            // Wrap the usage line in a code block, and add a yaml block example
            // This makes the usage of e.g. `register-appservice` more accurate
            let re = Regex::new(
                "(?m)^USAGE: {4}(.*?)

",
            )
            .expect("Regex compilation should not fail");
            re.replace_all(
                &text,
                "USAGE:\n<pre>$1[nobr]\n[commandbodyblock]</pre>",
            )
            .replace("[commandbodyblock]", &command_body)
        };

        // Add HTML line-breaks

        text.replace("\n\n\n", "\n\n")
            .replace('\n', "<br>\n")
            .replace("[nobr]<br>", "")
    }

    /// Create the admin room.
    ///
    /// Users in this room are considered admins by grapevine, and the room can
    /// be used to issue admin commands by talking to the server user inside
    /// it.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn create_admin_room(&self) -> Result<()> {
        let room_id = RoomId::new(services().globals.server_name());

        services().rooms.short.get_or_create_shortroomid(&room_id)?;

        let room_token = services()
            .globals
            .roomid_mutex_state
            .lock_key(room_id.clone())
            .await;

        // Create a user for the server
        let grapevine_user = get_grapevine_user();

        services().users.create(&grapevine_user, None)?;

        let room_version = services().globals.default_room_version();
        let mut content = match room_version {
            RoomVersionId::V1
            | RoomVersionId::V2
            | RoomVersionId::V3
            | RoomVersionId::V4
            | RoomVersionId::V5
            | RoomVersionId::V6
            | RoomVersionId::V7
            | RoomVersionId::V8
            | RoomVersionId::V9
            | RoomVersionId::V10 => RoomCreateEventContent::new_v1(
                services().globals.admin_bot_user_id.clone(),
            ),
            RoomVersionId::V11 => RoomCreateEventContent::new_v11(),
            _ => unreachable!("Validity of room version already checked"),
        };
        content.federate = true;
        content.predecessor = None;
        content.room_version = room_version;

        // 1. The room create event
        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomCreate,
                    content: to_raw_value(&content)
                        .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        // 2. Make grapevine bot join
        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomMember,
                    content: to_raw_value(&RoomMemberEventContent {
                        membership: MembershipState::Join,
                        displayname: None,
                        avatar_url: None,
                        is_direct: None,
                        third_party_invite: None,
                        blurhash: None,
                        reason: None,
                        join_authorized_via_users_server: None,
                    })
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(
                        services().globals.admin_bot_user_id.to_string(),
                    ),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        // 3. Power levels
        let mut users = BTreeMap::new();
        users.insert(services().globals.admin_bot_user_id.clone(), 100.into());

        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomPowerLevels,
                    content: to_raw_value(&RoomPowerLevelsEventContent {
                        users,
                        ..Default::default()
                    })
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        // 4.1 Join Rules
        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomJoinRules,
                    content: to_raw_value(&RoomJoinRulesEventContent::new(
                        JoinRule::Invite,
                    ))
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        // 4.2 History Visibility
        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomHistoryVisibility,
                    content: to_raw_value(
                        &RoomHistoryVisibilityEventContent::new(
                            HistoryVisibility::Shared,
                        ),
                    )
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        // 4.3 Guest Access
        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomGuestAccess,
                    content: to_raw_value(&RoomGuestAccessEventContent::new(
                        GuestAccess::Forbidden,
                    ))
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        // 5. Events implied by name and topic
        let room_name =
            format!("{} Admin Room", services().globals.server_name());
        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomName,
                    content: to_raw_value(&RoomNameEventContent::new(
                        room_name,
                    ))
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomTopic,
                    content: to_raw_value(&RoomTopicEventContent {
                        topic: format!(
                            "Manage {}",
                            services().globals.server_name()
                        ),
                    })
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        // 6. Room alias
        let alias = &services().globals.admin_bot_room_alias_id;

        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomCanonicalAlias,
                    content: to_raw_value(&RoomCanonicalAliasEventContent {
                        alias: Some(alias.clone()),
                        alt_aliases: Vec::new(),
                    })
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(String::new()),
                    redacts: None,
                },
                &services().globals.admin_bot_user_id,
                &room_token,
            )
            .await?;

        services().rooms.alias.set_alias(
            alias,
            &room_id,
            &services().globals.admin_bot_user_id,
        )?;

        Ok(())
    }

    /// Gets the room ID of the admin room
    ///
    /// Errors are propagated from the database, and will have None if there is
    /// no admin room
    // Allowed because this function uses `services()`
    #[allow(clippy::unused_self)]
    pub(crate) fn get_admin_room(&self) -> Result<Option<OwnedRoomId>> {
        services()
            .rooms
            .alias
            .resolve_local_alias(&services().globals.admin_bot_room_alias_id)
    }

    /// Invite the user to the grapevine admin room.
    ///
    /// In grapevine, this is equivalent to granting admin privileges.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn make_user_admin(
        &self,
        user_id: &UserId,
        displayname: String,
    ) -> Result<()> {
        if let Some(room_id) = services().admin.get_admin_room()? {
            let room_token = services()
                .globals
                .roomid_mutex_state
                .lock_key(room_id.clone())
                .await;

            // Use the server user to grant the new admin's power level
            let grapevine_user = get_grapevine_user();

            // Invite and join the real user
            services()
                .rooms
                .timeline
                .build_and_append_pdu(
                    PduBuilder {
                        event_type: TimelineEventType::RoomMember,
                        content: to_raw_value(&RoomMemberEventContent {
                            membership: MembershipState::Invite,
                            displayname: None,
                            avatar_url: None,
                            is_direct: None,
                            third_party_invite: None,
                            blurhash: None,
                            reason: None,
                            join_authorized_via_users_server: None,
                        })
                        .expect("event is valid, we just created it"),
                        unsigned: None,
                        state_key: Some(user_id.to_string()),
                        redacts: None,
                    },
                    &services().globals.admin_bot_user_id,
                    &room_token,
                )
                .await?;
            services()
                .rooms
                .timeline
                .build_and_append_pdu(
                    PduBuilder {
                        event_type: TimelineEventType::RoomMember,
                        content: to_raw_value(&RoomMemberEventContent {
                            membership: MembershipState::Join,
                            displayname: Some(displayname),
                            avatar_url: None,
                            is_direct: None,
                            third_party_invite: None,
                            blurhash: None,
                            reason: None,
                            join_authorized_via_users_server: None,
                        })
                        .expect("event is valid, we just created it"),
                        unsigned: None,
                        state_key: Some(user_id.to_string()),
                        redacts: None,
                    },
                    user_id,
                    &room_token,
                )
                .await?;

            // Set power level
            let mut users = BTreeMap::new();
            users.insert(
                services().globals.admin_bot_user_id.clone(),
                100.into(),
            );
            users.insert(user_id.to_owned(), 100.into());

            services()
                .rooms
                .timeline
                .build_and_append_pdu(
                    PduBuilder {
                        event_type: TimelineEventType::RoomPowerLevels,
                        content: to_raw_value(&RoomPowerLevelsEventContent {
                            users,
                            ..Default::default()
                        })
                        .expect("event is valid, we just created it"),
                        unsigned: None,
                        state_key: Some(String::new()),
                        redacts: None,
                    },
                    &services().globals.admin_bot_user_id,
                    &room_token,
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {}
