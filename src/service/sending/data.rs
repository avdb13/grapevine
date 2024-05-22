use ruma::ServerName;

use super::{OutgoingKind, RequestKey, SendingEventType};
use crate::Result;

pub(crate) trait Data: Send + Sync {
    #[allow(clippy::type_complexity)]
    fn active_requests<'a>(
        &'a self,
    ) -> Box<
        dyn Iterator<
                Item = Result<(RequestKey, OutgoingKind, SendingEventType)>,
            > + 'a,
    >;
    fn active_requests_for<'a>(
        &'a self,
        outgoing_kind: &OutgoingKind,
    ) -> Box<dyn Iterator<Item = Result<(RequestKey, SendingEventType)>> + 'a>;
    fn delete_active_request(&self, key: RequestKey) -> Result<()>;
    fn delete_all_active_requests_for(
        &self,
        outgoing_kind: &OutgoingKind,
    ) -> Result<()>;
    fn queue_requests(
        &self,
        requests: &[(&OutgoingKind, SendingEventType)],
    ) -> Result<Vec<RequestKey>>;
    fn queued_requests<'a>(
        &'a self,
        outgoing_kind: &OutgoingKind,
    ) -> Box<dyn Iterator<Item = Result<(SendingEventType, RequestKey)>> + 'a>;
    fn mark_as_active(
        &self,
        events: &[(SendingEventType, RequestKey)],
    ) -> Result<()>;
    fn set_latest_educount(
        &self,
        server_name: &ServerName,
        educount: u64,
    ) -> Result<()>;
    fn get_latest_educount(&self, server_name: &ServerName) -> Result<u64>;
}
