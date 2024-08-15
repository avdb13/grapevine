use clap::{command, Args, Subcommand};
use regex::RegexSet;
use ruma::{
    api::Direction, state_res::Event, MilliSecondsSinceUnixEpoch, OwnedRoomId,
    RoomId, UserId,
};

use crate::{
    service::rooms::timeline::PduCount,
    services,
    utils::query::{Query, Values},
    Result,
};

#[derive(Clone, Debug, Args)]
pub(crate) struct Room {
    pub(crate) room: OwnedRoomId,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct ListArgs {
    #[command(flatten)]
    pub(crate) query: Query,
    pub(crate) ordering: Ordering,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct RemoveArgs {
    pub(crate) rooms: Values<OwnedRoomId>,
    /// Remove users from their joined users
    pub(crate) leave_rooms: bool,
    /// Also deactivate admin accounts
    pub(crate) force: bool,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum Command {
    List(ListArgs),

    Deactivate(Room),

    Remove(RemoveArgs),

    #[command(subcommand)]
    /// Manage rooms' aliases
    Alias(alias::Command),

    #[command(subcommand)]
    /// Manage the room directory
    Directory(directory::Command),
}

pub(crate) mod alias {
    use clap::{Args, Subcommand};

    use super::Room;
    use crate::utils::query::Values;

    #[derive(Clone, Debug, Args)]
    pub(crate) struct SetArgs {
        #[arg(short, long)]
        /// Set the alias even if a room is already using it
        pub(crate) force: bool,

        #[command(flatten)]
        pub(crate) room: Room,

        pub(crate) alias: String,
    }

    #[derive(Clone, Debug, Args)]
    pub(crate) struct RemoveArgs {
        rooms: Values<String>,
    }

    #[derive(Clone, Debug, Subcommand)]
    pub(crate) enum Command {
        /// List aliases currently being used
        Get(Room),

        /// Make an alias point to a room.
        Set(SetArgs),

        /// Remove an alias
        Remove(RemoveArgs),

        /// Show which room is using an alias
        Resolve(Room),
    }
}

pub(crate) mod directory {
    use clap::Subcommand;

    use super::{ListArgs, Room};

    #[derive(Clone, Debug, Subcommand)]
    pub(crate) enum Command {
        /// List rooms that are published
        List(ListArgs),

        /// Publish a room to the room directory
        Publish(Room),

        /// Unpublish a room to the room directory
        Unpublish(Room),
    }
}

pub(crate) mod federation {
    use clap::Subcommand;

    use super::Room;

    #[derive(Clone, Debug, Subcommand)]
    pub(crate) enum Command {
        // /// List all rooms we are currently handling an incoming pdu from
        // IncomingFederation,
        /// Enables incoming federation handling for a room.
        Enable(Room),

        /// Disables incoming federation handling for a room.
        Disable(Room),
    }
}

pub(crate) fn list(
    query: Query,
    ordering: Ordering,
) -> Result<Vec<OwnedRoomId>> {
    let Query {
        patterns,
        offset,
        limit,
        direction,
    } = query;

    let patterns: Vec<_> = patterns
        .iter()
        // wildcard_to_regex(s)
        .filter_map(|s| RegexSet::new([s]).ok())
        .collect();

    let ids = services().rooms.metadata.iter_ids().filter(|room_id| {
        if let (v @ [_, ..], Ok(room_id)) =
            (patterns.as_slice(), room_id.as_deref())
        {
            v.iter().any(|pattern| pattern.is_match(&get_name(room_id)))
        } else {
            true
        }
    });

    let mut rooms: Vec<_> =
        ids.skip(offset).take(limit).collect::<Result<_>>()?;

    match ordering {
        Ordering::Name => {
            rooms.sort_by_key(|room_id| get_name(room_id));
        }
        Ordering::JoinedMembers => {
            rooms.sort_by_key(|room_id| {
                get_joined_members(room_id).unwrap_or(0)
            });
        }
        Ordering::Timestamp => {
            rooms.sort_by_key(|room_id| {
                get_last_pdu(room_id)
                    .ok()
                    .flatten()
                    .unwrap_or(MilliSecondsSinceUnixEpoch(ruma::uint!(0)))
            });
        }
    }

    if direction == Direction::Backward {
        rooms.reverse();
    }

    Ok(rooms)
}

fn get_name(room_id: &RoomId) -> String {
    match services().rooms.state_accessor.get_name(room_id) {
        Ok(Some(name)) => name,
        _ => {
            let suffix = room_id
                .server_name()
                .map(|server_name| format!(":{server_name}"));

            format!("{room_id}")
                .trim_end_matches(&suffix.unwrap_or_default())
                .to_owned()
        }
    }
}

fn get_joined_members(room_id: &RoomId) -> Result<u64> {
    match services().rooms.state_cache.room_joined_count(room_id)? {
        Some(n) => Ok(n),
        _ => Ok(0),
    }
}

fn get_last_pdu(
    room_id: &RoomId,
) -> Result<Option<MilliSecondsSinceUnixEpoch>> {
    let user_id =
        UserId::parse_with_server_name("", services().globals.server_name());
    let user_id = user_id.as_ref().expect("we know this is valid");

    let pdus =
        services().rooms.timeline.pdus_until(user_id, room_id, PduCount::MAX);
    let (_, pdu) =
        pdus.and_then(|pdus| pdus.last().transpose()).map(Option::unzip)?;

    Ok(pdu.as_ref().map(Event::origin_server_ts))
}

#[derive(Clone, Debug, clap::ValueEnum, clap::Subcommand)]
pub(crate) enum Ordering {
    Name,
    Timestamp,
    JoinedMembers,
}

// #[derive(Clone, Debug)]
// pub(crate) struct RoomDetails {
//     pub room: OwnedRoomId,
//     pub version: RoomVersionId,
//     pub creator: OwnedUserId,

//     pub kind: RoomType,
//     pub join_rule: JoinRule,
//     pub history_visibility: HistoryVisibility,

//     pub alias: Option<OwnedRoomAliasId>,
//     pub avatar_url: Option<OwnedMxcUri>,
//     pub summary: Option<RoomSummary>,

//     pub federated: bool,
//     pub published: bool,
// }
