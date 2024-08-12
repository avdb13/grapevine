use std::{collections::BTreeMap, fmt::Write, sync::Arc, time::Instant};

use clap::ValueEnum;
use regex::Regex;
use ruma::{
    api::appservice::Registration,
    events::{
        push_rules::{PushRulesEvent, PushRulesEventContent},
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
    signatures::verify_json,
    EventId, MilliSecondsSinceUnixEpoch, OwnedRoomId, RoomId, RoomVersionId,
    ServerName, UserId,
};
use serde_json::value::to_raw_value;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::warn;

use self::command::Command;
use super::pdu::PduBuilder;
use crate::{
    api::client_server::{leave_all_rooms, AUTO_GEN_PASSWORD_LENGTH},
    services,
    utils::{self, dbg_truncate_str},
    Error, PduEvent, Result,
};

mod command;

#[derive(Debug)]
pub(crate) enum AdminRoomEvent {
    ProcessMessage(String, Arc<EventId>),
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
            })
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

    // Parse chat messages from the admin room into an AdminCommand object
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

        AdminCommand::try_parse_from(argv).map_err(|error| error.to_string())
    }

    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self, body))]
    async fn process_admin_command(
        &self,
        command: AdminCommand,
        body: Vec<&str>,
    ) -> Result<RoomMessageEventContent> {
        let reply_message_content = match command {
            AdminCommand::RegisterAppservice => {
                if body.len() > 2
                    && body[0].trim() == "```"
                    && body.last().unwrap().trim() == "```"
                {
                    let appservice_config = body[1..body.len() - 1].join("\n");
                    let parsed_config = serde_yaml::from_str::<Registration>(
                        &appservice_config,
                    );
                    match parsed_config {
                        Ok(yaml) => match services()
                            .appservice
                            .register_appservice(yaml)
                            .await
                        {
                            Ok(id) => RoomMessageEventContent::text_plain(
                                format!("Appservice registered with ID: {id}."),
                            ),
                            Err(e) => RoomMessageEventContent::text_plain(
                                format!("Failed to register appservice: {e}"),
                            ),
                        },
                        Err(e) => RoomMessageEventContent::text_plain(format!(
                            "Could not parse appservice config: {e}"
                        )),
                    }
                } else {
                    RoomMessageEventContent::text_plain(
                        "Expected code block in command body. Add --help for \
                         details.",
                    )
                }
            }
            AdminCommand::UnregisterAppservice {
                appservice_identifier,
            } => match services()
                .appservice
                .unregister_appservice(&appservice_identifier)
                .await
            {
                Ok(()) => RoomMessageEventContent::text_plain(
                    "Appservice unregistered.",
                ),
                Err(e) => RoomMessageEventContent::text_plain(format!(
                    "Failed to unregister appservice: {e}"
                )),
            },
            AdminCommand::ListAppservices => {
                let appservices = services().appservice.iter_ids().await;
                let output = format!(
                    "Appservices ({}): {}",
                    appservices.len(),
                    appservices.join(", ")
                );
                RoomMessageEventContent::text_plain(output)
            }
            AdminCommand::ListRooms => {
                let room_ids = services().rooms.metadata.iter_ids();
                let output = format!(
                    "Rooms:\n{}",
                    room_ids
                        .filter_map(std::result::Result::ok)
                        .map(|id| format!(
                            "{id}\tMembers: {}",
                            &services()
                                .rooms
                                .state_cache
                                .room_joined_count(&id)
                                .ok()
                                .flatten()
                                .unwrap_or(0)
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                RoomMessageEventContent::text_plain(output)
            }
            AdminCommand::ListLocalUsers => match services()
                .users
                .list_local_users()
            {
                Ok(users) => {
                    let mut msg: String = format!(
                        "Found {} local user account(s):\n",
                        users.len()
                    );
                    msg += &users.join("\n");
                    RoomMessageEventContent::text_plain(&msg)
                }
                Err(e) => RoomMessageEventContent::text_plain(e.to_string()),
            },
            AdminCommand::IncomingFederation => {
                let map =
                    services().globals.roomid_federationhandletime.read().await;
                let mut msg: String =
                    format!("Handling {} incoming pdus:\n", map.len());

                for (r, (e, i)) in map.iter() {
                    let elapsed = i.elapsed();
                    writeln!(
                        msg,
                        "{r} {e}: {}m{}s",
                        elapsed.as_secs() / 60,
                        elapsed.as_secs() % 60
                    )
                    .expect("write to in-memory buffer should succeed");
                }
                RoomMessageEventContent::text_plain(&msg)
            }
            AdminCommand::GetAuthChain {
                event_id,
            } => {
                let event_id = Arc::<EventId>::from(event_id);
                if let Some(event) =
                    services().rooms.timeline.get_pdu_json(&event_id)?
                {
                    let room_id_str = event
                        .get("room_id")
                        .and_then(|val| val.as_str())
                        .ok_or_else(|| {
                            Error::bad_database("Invalid event in database")
                        })?;

                    let room_id =
                        <&RoomId>::try_from(room_id_str).map_err(|_| {
                            Error::bad_database(
                                "Invalid room id field in event in database",
                            )
                        })?;
                    let start = Instant::now();
                    let count = services()
                        .rooms
                        .auth_chain
                        .get_auth_chain(room_id, vec![event_id])
                        .await?
                        .count();
                    let elapsed = start.elapsed();
                    RoomMessageEventContent::text_plain(format!(
                        "Loaded auth chain with length {count} in {elapsed:?}"
                    ))
                } else {
                    RoomMessageEventContent::text_plain("Event not found.")
                }
            }
            AdminCommand::ParsePdu => {
                if body.len() > 2
                    && body[0].trim() == "```"
                    && body.last().unwrap().trim() == "```"
                {
                    let string = body[1..body.len() - 1].join("\n");
                    match serde_json::from_str(&string) {
                        Ok(value) => {
                            match ruma::signatures::reference_hash(
                                &value,
                                &RoomVersionId::V6,
                            ) {
                                Ok(hash) => {
                                    let event_id =
                                        EventId::parse(format!("${hash}"));

                                    match serde_json::from_value::<PduEvent>(
                                        serde_json::to_value(value)
                                            .expect("value is json"),
                                    ) {
                                        Ok(pdu) => {
                                            RoomMessageEventContent::text_plain(
                                                format!(
                                                    "EventId: {event_id:?}\\
                                                     n{pdu:#?}"
                                                ),
                                            )
                                        }
                                        Err(e) => {
                                            RoomMessageEventContent::text_plain(
                                                format!(
                                                    "EventId: {event_id:?}\\
                                                     nCould not parse event: \
                                                     {e}"
                                                ),
                                            )
                                        }
                                    }
                                }
                                Err(e) => RoomMessageEventContent::text_plain(
                                    format!("Could not parse PDU JSON: {e:?}"),
                                ),
                            }
                        }
                        Err(e) => RoomMessageEventContent::text_plain(format!(
                            "Invalid json in command body: {e}"
                        )),
                    }
                } else {
                    RoomMessageEventContent::text_plain(
                        "Expected code block in command body.",
                    )
                }
            }
            AdminCommand::GetPdu {
                event_id,
            } => {
                let mut outlier = false;
                let mut pdu_json = services()
                    .rooms
                    .timeline
                    .get_non_outlier_pdu_json(&event_id)?;
                if pdu_json.is_none() {
                    outlier = true;
                    pdu_json =
                        services().rooms.timeline.get_pdu_json(&event_id)?;
                }
                match pdu_json {
                    Some(json) => {
                        let json_text = serde_json::to_string_pretty(&json)
                            .expect("canonical json is valid json");
                        RoomMessageEventContent::text_html(
                            format!(
                                "{}\n```json\n{}\n```",
                                if outlier {
                                    "PDU is outlier"
                                } else {
                                    "PDU was accepted"
                                },
                                json_text
                            ),
                            format!(
                                "<p>{}</p>\n<pre><code \
                                 class=\"language-json\">{}\n</code></pre>\n",
                                if outlier {
                                    "PDU is outlier"
                                } else {
                                    "PDU was accepted"
                                },
                                html_escape::encode_safe(&json_text)
                            ),
                        )
                    }
                    None => {
                        RoomMessageEventContent::text_plain("PDU not found.")
                    }
                }
            }
            AdminCommand::MemoryUsage => {
                let response1 = services().memory_usage().await;
                let response2 = services().globals.db.memory_usage();

                RoomMessageEventContent::text_plain(format!(
                    "Services:\n{response1}\n\nDatabase:\n{response2}"
                ))
            }
            AdminCommand::ClearDatabaseCaches {
                amount,
            } => {
                services().globals.db.clear_caches(amount);

                RoomMessageEventContent::text_plain("Done.")
            }
            AdminCommand::ClearServiceCaches {
                amount,
            } => {
                services().clear_caches(amount).await;

                RoomMessageEventContent::text_plain("Done.")
            }
            AdminCommand::ResetPassword {
                username,
            } => {
                let user_id = match UserId::parse_with_server_name(
                    username.as_str().to_lowercase(),
                    services().globals.server_name(),
                ) {
                    Ok(id) => id,
                    Err(e) => {
                        return Ok(RoomMessageEventContent::text_plain(
                            format!(
                                "The supplied username is not a valid \
                                 username: {e}"
                            ),
                        ))
                    }
                };

                // Checks if user is local
                if user_id.server_name() != services().globals.server_name() {
                    return Ok(RoomMessageEventContent::text_plain(
                        "The specified user is not from this server!",
                    ));
                };

                // Check if the specified user is valid
                if !services().users.exists(&user_id)?
                    || user_id == services().globals.admin_bot_user_id
                {
                    return Ok(RoomMessageEventContent::text_plain(
                        "The specified user does not exist!",
                    ));
                }

                let new_password =
                    utils::random_string(AUTO_GEN_PASSWORD_LENGTH);

                match services()
                    .users
                    .set_password(&user_id, Some(new_password.as_str()))
                {
                    Ok(()) => RoomMessageEventContent::text_plain(format!(
                        "Successfully reset the password for user {user_id}: \
                         {new_password}"
                    )),
                    Err(e) => RoomMessageEventContent::text_plain(format!(
                        "Couldn't reset the password for user {user_id}: {e}"
                    )),
                }
            }
            AdminCommand::CreateUser {
                username,
                password,
            } => {
                let password = password.unwrap_or_else(|| {
                    utils::random_string(AUTO_GEN_PASSWORD_LENGTH)
                });
                // Validate user id
                let user_id = match UserId::parse_with_server_name(
                    username.as_str().to_lowercase(),
                    services().globals.server_name(),
                ) {
                    Ok(id) => id,
                    Err(e) => {
                        return Ok(RoomMessageEventContent::text_plain(
                            format!(
                                "The supplied username is not a valid \
                                 username: {e}"
                            ),
                        ))
                    }
                };
                if user_id.is_historical() {
                    return Ok(RoomMessageEventContent::text_plain(format!(
                        "Userid {user_id} is not allowed due to historical"
                    )));
                }
                if services().users.exists(&user_id)? {
                    return Ok(RoomMessageEventContent::text_plain(format!(
                        "Userid {user_id} already exists"
                    )));
                }
                // Create user
                services().users.create(&user_id, Some(password.as_str()))?;

                // Default to pretty displayname
                let displayname = user_id.localpart().to_owned();

                services()
                    .users
                    .set_displayname(&user_id, Some(displayname))?;

                // Initial account data
                services().account_data.update(
                    None,
                    &user_id,
                    ruma::events::GlobalAccountDataEventType::PushRules
                        .to_string()
                        .into(),
                    &serde_json::to_value(PushRulesEvent {
                        content: PushRulesEventContent {
                            global: ruma::push::Ruleset::server_default(
                                &user_id,
                            ),
                        },
                    })
                    .expect("to json value always works"),
                )?;

                // we dont add a device since we're not the user, just the
                // creator

                // Inhibit login does not work for guests
                RoomMessageEventContent::text_plain(format!(
                    "Created user with user_id: {user_id} and password: \
                     {password}"
                ))
            }
            AdminCommand::DisableRoom {
                room_id,
            } => {
                services().rooms.metadata.disable_room(&room_id, true)?;
                RoomMessageEventContent::text_plain("Room disabled.")
            }
            AdminCommand::EnableRoom {
                room_id,
            } => {
                services().rooms.metadata.disable_room(&room_id, false)?;
                RoomMessageEventContent::text_plain("Room enabled.")
            }
            AdminCommand::DeactivateUser {
                leave_rooms,
                user_id,
            } => {
                let user_id = Arc::<UserId>::from(user_id);
                if !services().users.exists(&user_id)? {
                    RoomMessageEventContent::text_plain(format!(
                        "User {user_id} doesn't exist on this server"
                    ))
                } else if user_id.server_name()
                    != services().globals.server_name()
                {
                    RoomMessageEventContent::text_plain(format!(
                        "User {user_id} is not from this server"
                    ))
                } else {
                    RoomMessageEventContent::text_plain(format!(
                        "Making {user_id} leave all rooms before \
                         deactivation..."
                    ));

                    services().users.deactivate_account(&user_id)?;

                    if leave_rooms {
                        leave_all_rooms(&user_id).await?;
                    }

                    RoomMessageEventContent::text_plain(format!(
                        "User {user_id} has been deactivated"
                    ))
                }
            }
            AdminCommand::DeactivateAll {
                leave_rooms,
                force,
            } => {
                if body.len() > 2
                    && body[0].trim() == "```"
                    && body.last().unwrap().trim() == "```"
                {
                    let users = body
                        .clone()
                        .drain(1..body.len() - 1)
                        .collect::<Vec<_>>();

                    let mut user_ids = Vec::new();
                    let mut remote_ids = Vec::new();
                    let mut non_existant_ids = Vec::new();
                    let mut invalid_users = Vec::new();

                    for &user in &users {
                        match <&UserId>::try_from(user) {
                            Ok(user_id) => {
                                if user_id.server_name()
                                    != services().globals.server_name()
                                {
                                    remote_ids.push(user_id);
                                } else if !services().users.exists(user_id)? {
                                    non_existant_ids.push(user_id);
                                } else {
                                    user_ids.push(user_id);
                                }
                            }
                            Err(_) => {
                                invalid_users.push(user);
                            }
                        }
                    }

                    let mut markdown_message = String::new();
                    let mut html_message = String::new();
                    if !invalid_users.is_empty() {
                        markdown_message.push_str(
                            "The following user ids are not valid:\n```\n",
                        );
                        html_message.push_str(
                            "The following user ids are not valid:\n<pre>\n",
                        );
                        for invalid_user in invalid_users {
                            writeln!(markdown_message, "{invalid_user}")
                                .expect(
                                    "write to in-memory buffer should succeed",
                                );
                            writeln!(html_message, "{invalid_user}").expect(
                                "write to in-memory buffer should succeed",
                            );
                        }
                        markdown_message.push_str("```\n\n");
                        html_message.push_str("</pre>\n\n");
                    }
                    if !remote_ids.is_empty() {
                        markdown_message.push_str(
                            "The following users are not from this \
                             server:\n```\n",
                        );
                        html_message.push_str(
                            "The following users are not from this \
                             server:\n<pre>\n",
                        );
                        for remote_id in remote_ids {
                            writeln!(markdown_message, "{remote_id}").expect(
                                "write to in-memory buffer should succeed",
                            );
                            writeln!(html_message, "{remote_id}").expect(
                                "write to in-memory buffer should succeed",
                            );
                        }
                        markdown_message.push_str("```\n\n");
                        html_message.push_str("</pre>\n\n");
                    }
                    if !non_existant_ids.is_empty() {
                        markdown_message.push_str(
                            "The following users do not exist:\n```\n",
                        );
                        html_message.push_str(
                            "The following users do not exist:\n<pre>\n",
                        );
                        for non_existant_id in non_existant_ids {
                            writeln!(markdown_message, "{non_existant_id}")
                                .expect(
                                    "write to in-memory buffer should succeed",
                                );
                            writeln!(html_message, "{non_existant_id}").expect(
                                "write to in-memory buffer should succeed",
                            );
                        }
                        markdown_message.push_str("```\n\n");
                        html_message.push_str("</pre>\n\n");
                    }
                    if !markdown_message.is_empty() {
                        return Ok(RoomMessageEventContent::text_html(
                            markdown_message,
                            html_message,
                        ));
                    }

                    let mut deactivation_count = 0;
                    let mut admins = Vec::new();

                    if !force {
                        user_ids.retain(|&user_id| {
                            match services().users.is_admin(user_id) {
                                Ok(is_admin) => {
                                    if is_admin {
                                        admins.push(user_id.localpart());
                                        false
                                    } else {
                                        true
                                    }
                                }
                                Err(_) => false,
                            }
                        });
                    }

                    for &user_id in &user_ids {
                        if services().users.deactivate_account(user_id).is_ok()
                        {
                            deactivation_count += 1;
                        }
                    }

                    if leave_rooms {
                        for &user_id in &user_ids {
                            if let Err(error) = leave_all_rooms(user_id).await {
                                warn!(%user_id, %error, "failed to leave one or more rooms");
                            }
                        }
                    }

                    if admins.is_empty() {
                        RoomMessageEventContent::text_plain(format!(
                            "Deactivated {deactivation_count} accounts."
                        ))
                    } else {
                        RoomMessageEventContent::text_plain(format!(
                            "Deactivated {} accounts.\nSkipped admin \
                             accounts: {:?}. Use --force to deactivate admin \
                             accounts",
                            deactivation_count,
                            admins.join(", ")
                        ))
                    }
                } else {
                    RoomMessageEventContent::text_plain(
                        "Expected code block in command body. Add --help for \
                         details.",
                    )
                }
            }
            AdminCommand::SignJson => {
                if body.len() > 2
                    && body[0].trim() == "```"
                    && body.last().unwrap().trim() == "```"
                {
                    let string = body[1..body.len() - 1].join("\n");
                    match serde_json::from_str(&string) {
                        Ok(mut value) => {
                            ruma::signatures::sign_json(
                                services().globals.server_name().as_str(),
                                services().globals.keypair(),
                                &mut value,
                            )
                            .expect("our request json is what ruma expects");
                            let json_text =
                                serde_json::to_string_pretty(&value)
                                    .expect("canonical json is valid json");
                            RoomMessageEventContent::text_plain(json_text)
                        }
                        Err(e) => RoomMessageEventContent::text_plain(format!(
                            "Invalid json: {e}"
                        )),
                    }
                } else {
                    RoomMessageEventContent::text_plain(
                        "Expected code block in command body. Add --help for \
                         details.",
                    )
                }
            }
            AdminCommand::VerifyJson => {
                if body.len() > 2
                    && body[0].trim() == "```"
                    && body.last().unwrap().trim() == "```"
                {
                    let string = body[1..body.len() - 1].join("\n");
                    match serde_json::from_str(&string) {
                        Ok(value) => {
                            let pub_key_map = RwLock::new(BTreeMap::new());

                            services()
                                .rooms
                                .event_handler
                                // Generally we shouldn't be checking against
                                // expired keys unless required, so in the admin
                                // room it might be best to not allow expired
                                // keys
                                .fetch_required_signing_keys(
                                    &value,
                                    &pub_key_map
                                )
                                .await?;

                            let mut expired_key_map = BTreeMap::new();
                            let mut valid_key_map = BTreeMap::new();

                            for (server, keys) in pub_key_map.into_inner() {
                                if keys.valid_until_ts
                                    > MilliSecondsSinceUnixEpoch::now()
                                {
                                    valid_key_map.insert(
                                        server,
                                        keys.verify_keys
                                            .into_iter()
                                            .map(|(id, key)| (id, key.key))
                                            .collect(),
                                    );
                                } else {
                                    expired_key_map.insert(
                                        server,
                                        keys.verify_keys
                                            .into_iter()
                                            .map(|(id, key)| (id, key.key))
                                            .collect(),
                                    );
                                }
                            }

                            if verify_json(&valid_key_map, &value).is_ok() {
                                RoomMessageEventContent::text_plain(
                                    "Signature correct",
                                )
                            } else if let Err(e) =
                                verify_json(&expired_key_map, &value)
                            {
                                RoomMessageEventContent::text_plain(format!(
                                    "Signature verification failed: {e}"
                                ))
                            } else {
                                RoomMessageEventContent::text_plain(
                                    "Signature correct (with expired keys)",
                                )
                            }
                        }
                        Err(e) => RoomMessageEventContent::text_plain(format!(
                            "Invalid json: {e}"
                        )),
                    }
                } else {
                    RoomMessageEventContent::text_plain(
                        "Expected code block in command body. Add --help for \
                         details.",
                    )
                }
            }
            AdminCommand::SetTracingFilter {
                backend,
                filter,
            } => {
                let handles = &services().globals.reload_handles;
                let handle = match backend {
                    TracingBackend::Log => &handles.log,
                    TracingBackend::Flame => &handles.flame,
                    TracingBackend::Traces => &handles.traces,
                };
                let Some(handle) = handle else {
                    return Ok(RoomMessageEventContent::text_plain(
                        "Backend is disabled",
                    ));
                };
                let filter = match filter.parse() {
                    Ok(filter) => filter,
                    Err(e) => {
                        return Ok(RoomMessageEventContent::text_plain(
                            format!("Invalid filter string: {e}"),
                        ));
                    }
                };
                if let Err(e) = handle.reload(filter) {
                    return Ok(RoomMessageEventContent::text_plain(format!(
                        "Failed to reload filter: {e}"
                    )));
                };

                return Ok(RoomMessageEventContent::text_plain(
                    "Filter reloaded",
                ));
            }
        };

        Ok(reply_message_content)
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
        let error =
            AdminCommand::try_parse_from(["argv[0] doesn't matter", input])
                .unwrap_err()
                .to_string();

        // Search for a handful of keywords that suggest the help printed
        // properly
        assert!(error.contains("Usage:"));
        assert!(error.contains("Commands:"));
        assert!(error.contains("Options:"));
    }
}
