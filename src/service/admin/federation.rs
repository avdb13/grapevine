use ruma::OwnedEventId;

use crate::utils::query::Values;

#[derive(Debug, clap::Subcommand)]
pub(crate) enum Command {
    /// Get the `auth_chain` of a PDU
    AuthChain {
        /// An event ID (the $ character followed by the base64 reference hash)
        /// An event ID (a $ followed by the base64 reference hash)
        events: Values<OwnedEventId>,
    },
}

pub(crate) mod json {
    #[derive(Debug, clap::Subcommand)]
    pub(crate) enum Command {
        // /// List all rooms we are currently handling an incoming pdu from
        // List,
        #[command(verbatim_doc_comment)]
        /// Verify json signatures
        /// [commandbody]
        /// # ```
        /// # json here
        /// # ```
        Sign,

        #[command(verbatim_doc_comment)]
        /// Verify json signatures
        /// [commandbody]
        /// # ```
        /// # json here
        /// # ```
        Verify,
    }
}
