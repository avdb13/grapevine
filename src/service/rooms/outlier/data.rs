use ruma::{CanonicalJsonObject, EventId};

use crate::{PduEvent, Result};

pub(crate) trait Data: Send + Sync {
    /// Returns the pdu from the outlier tree.
    fn get_outlier_pdu_json(&self, event_id: &EventId) -> Result<Option<CanonicalJsonObject>>;
    fn get_outlier_pdu(&self, event_id: &EventId) -> Result<Option<PduEvent>>;
    /// Append the PDU as an outlier.
    fn add_pdu_outlier(&self, event_id: &EventId, pdu: &CanonicalJsonObject) -> Result<()>;
}
