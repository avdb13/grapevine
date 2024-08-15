use clap::{Args, Subcommand};
use ruma::OwnedUserId;

use crate::utils::query::{Query, Values};

#[derive(Clone, Debug, Args)]
pub(crate) struct User {
    user: OwnedUserId,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct ListArgs {
    #[clap(flatten)]
    pub(crate) query: Query,
    pub(crate) ordering: Ordering,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct RemoveArgs {
    pub(crate) users: Values<OwnedUserId>,
    /// Remove users from their joined users
    pub(crate) leave_rooms: bool,
    /// Also deactivate admin accounts
    pub(crate) force: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// List users in the database
    List(ListArgs),

    Deactivate(User),

    /// Deactivate or delete a user
    ///
    /// User will not be removed from all users by default.
    /// Use --leave-users to force the user to leave all users
    Remove(RemoveArgs),
}

#[derive(Clone, Debug, clap::ValueEnum, clap::Subcommand)]
pub(crate) enum Ordering {
    Displayname,
    Timestamp,
    Messages,
}

// #[derive(Clone, Debug)]
// pub(crate) struct Details {
//     user: OwnedUserId,
//     password: Option<String>,

//     updated: u64,
//     created: u64,

//     displayname: Option<String>,
//     avatar_url: Option<OwnedMxcUri>,
//     devices: HashSet<Device>,

//     deactivated: bool,
//     admin: bool,
// }
