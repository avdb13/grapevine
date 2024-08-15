use std::{collections::BTreeMap, sync::Arc};

use clap::{Parser, ValueEnum};
use regex::Regex;
use ruma::{
    events::{
        relation::InReplyTo,
        room::{
            canonical_alias::RoomCanonicalAliasEventContent,
            create::RoomCreateEventContent,
            guest_access::{GuestAccess, RoomGuestAccessEventContent},
            history_visibility::{
                HistoryVisibility, RoomHistoryVisibilityEventContent,
            },
            join_rules::{JoinRule, RoomJoinRulesEventContent},
            member::{MembershipState, RoomMemberEventContent},
            message::{Relation, RoomMessageEventContent},
            name::RoomNameEventContent,
            power_levels::RoomPowerLevelsEventContent,
            topic::RoomTopicEventContent,
        },
        TimelineEventType,
    },
    EventId, OwnedRoomId, RoomId, RoomVersionId, ServerName, UserId,
};
use serde_json::value::to_raw_value;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

use super::pdu::PduBuilder;
use crate::{services, utils::dbg_truncate_str, Error, Result};

mod appservices;
mod federation;
mod rooms;
mod server;
mod users;

#[derive(Debug, Parser)]
#[command(name = "!admin", version = env!("CARGO_PKG_VERSION"))]
pub(crate) enum Command {
    #[command(subcommand)]
    /// Commands for managing the server
    Server(server::Command),

    #[command(subcommand)]
    /// Commands for managing rooms
    Rooms(rooms::Command),

    #[command(subcommand)]
    /// Commands for managing local users
    Users(users::Command),

    #[command(subcommand)]
    /// Commands for managing appservices
    Appservices(appservices::Command),
}

#[derive(Debug)]
pub(crate) enum AdminRoomEvent {
    ProcessMessage(String, Arc<EventId>),
    SendMessage(RoomMessageEventContent),
}

pub(crate) struct Service {
    pub(crate) sender: mpsc::UnboundedSender<AdminRoomEvent>,
    receiver: Mutex<mpsc::UnboundedReceiver<AdminRoomEvent>>,
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

    #[tracing::instrument(skip(self, grapevine_room))]
    async fn handle_event(
        &self,
        event: AdminRoomEvent,
        grapevine_room: &OwnedRoomId,
    ) {
        let message_content = match event {
            AdminRoomEvent::SendMessage(content) => content,
            AdminRoomEvent::ProcessMessage(room_message, event_id) => {
                self.process_admin_message(room_message, event_id).await
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
    pub(crate) fn process_message(
        &self,
        room_message: String,
        event_id: Arc<EventId>,
    ) {
        self.sender
            .send(AdminRoomEvent::ProcessMessage(room_message, event_id))
            .unwrap();
    }

    #[tracing::instrument(skip(self, message_content))]
    pub(crate) fn send_message(
        &self,
        mut message_content: RoomMessageEventContent,
        reply: Option<Arc<EventId>>,
    ) {
        if let Some(in_reply_to) =
            reply.as_deref().map(EventId::to_owned).map(InReplyTo::new)
        {
            message_content.relates_to = Some(Relation::Reply {
                in_reply_to,
            });
        }

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
        event_id: Arc<EventId>,
    ) -> RoomMessageEventContent {
        let mut lines = room_message.lines().filter(|l| !l.trim().is_empty());
        let command_line =
            lines.next().expect("each string has at least one line");
        let body: Vec<_> = lines.collect();

        let admin_command = match Self::parse_admin_command(command_line) {
            Ok(command) => command,
            Err(error) => {
                let server_name = services().globals.server_name();
                let message =
                    error.replace("server.name", server_name.as_str());
                let html_message = Self::usage_to_html(&message, server_name);

                return RoomMessageEventContent::text_html(
                    message,
                    html_message,
                );
            }
        };

        match self.process_admin_command(admin_command, body).await {
            Ok(reply_message) => reply_message,
            Err(error) => {
                let markdown_message = format!(
                    "Encountered an error while handling the \
                     command:\n```\n{error}\n```",
                );
                let html_message = format!(
                    "Encountered an error while handling the \
                     command:\n<pre>\n{error}\n</pre>",
                );

                RoomMessageEventContent::text_html(
                    markdown_message,
                    html_message,
                )
            }
        }
    }

    // Parse chat messages from the admin room into an Command object
    #[tracing::instrument(
        skip(command_line),
        fields(
            command_line = dbg_truncate_str(command_line, 50).as_ref(),
        ),
    )]
    fn parse_admin_command(
        command_line: &str,
    ) -> std::result::Result<Command, String> {
        // Note: argv[0] is `@grapevine:servername:`, which is treated as the
        // main command
        let mut argv: Vec<_> = command_line.split_whitespace().collect();

        // Replace `help command` with `command --help`
        // Clap has a help subcommand, but it omits the long help description.
        if argv.len() > 1 && argv[1] == "help" {
            argv.remove(1);
            argv.push("--help");
        }

        // Backwards compatibility with `register_appservice`-style commands
        let command_with_dashes;
        if argv.len() > 1 && argv[1].contains('_') {
            command_with_dashes = argv[1].replace('_', "-");
            argv[1] = &command_with_dashes;
        }

        Parser::try_parse_from(argv).map_err(|error| error.to_string())
    }

    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self, body))]
    async fn process_admin_command(
        &self,
        command: Command,
        body: Vec<&str>,
    ) -> Result<RoomMessageEventContent> {
        todo!()
    }

    // Utility to turn clap's `--help` text to HTML.
    #[tracing::instrument(skip_all)]
    fn usage_to_html(text: &str, server_name: &ServerName) -> String {
        // Replace `@grapevine:servername:-subcmdname` with
        // `@grapevine:servername: subcmdname`
        let localpart = services().globals.admin_bot_user_id.localpart();

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
        let re = Regex::new("(?m)^    (([a-zA-Z_&;-]+(, )?)+)  +(.*)$")
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
            let re = Regex::new("(?m)^USAGE:\n    (@grapevine:.*)$")
                .expect("Regex compilation should not fail");
            re.replace_all(&text, "USAGE:\n<code>$1</code>").to_string()
        } else {
            // Wrap the usage line in a code block, and add a yaml block example
            // This makes the usage of e.g. `register-appservice` more accurate
            let re = Regex::new("(?m)^USAGE:\n    (.*?)\n\n")
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
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self))]
    pub(crate) async fn create_admin_room(&self) -> Result<()> {
        let room_id = RoomId::new(services().globals.server_name());

        services().rooms.short.get_or_create_shortroomid(&room_id)?;

        let room_token = services()
            .globals
            .roomid_mutex_state
            .lock_key(room_id.clone())
            .await;

        services().users.create(&services().globals.admin_bot_user_id, None)?;

        let room_version = services().globals.default_room_version();
        let mut content = match &room_version {
            room_version if *room_version < RoomVersionId::V11 => {
                RoomCreateEventContent::new_v1(
                    services().globals.admin_bot_user_id.clone(),
                )
            }
            RoomVersionId::V11 => RoomCreateEventContent::new_v11(),
            _ => {
                return Err(Error::BadServerResponse(
                    "Unsupported room version.",
                ))
            }
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
mod test {
    use super::*;

    #[test]
    fn get_help_short() {
        get_help_inner("-h");
    }

    #[test]
    fn get_help_long() {
        get_help_inner("--help");
    }

    #[test]
    fn get_help_subcommand() {
        get_help_inner("help");
    }

    fn get_help_inner(input: &str) {
        let error = Command::try_parse_from(["argv[0] doesn't matter", input])
            .unwrap_err()
            .to_string();

        // Search for a handful of keywords that suggest the help printed
        // properly
        assert!(error.contains("Usage:"));
        assert!(error.contains("Commands:"));
        assert!(error.contains("Options:"));
    }
}

#[derive(Debug, Clone, ValueEnum)]
enum TracingBackend {
    Log,
    Flame,
    Traces,
}
