use ruma::{CanonicalJsonObject, EventId};

use crate::{database::KeyValueDatabase, service, Error, Result};

impl service::rooms::outlier::Data for KeyValueDatabase {
    fn get_outlier_pdu_json(
        &self,
        event_id: &EventId,
    ) -> Result<Option<CanonicalJsonObject>> {
        self.eventid_outlierpdu.get(event_id.as_bytes())?.map_or(
            Ok(None),
            |pdu| {
                serde_json::from_slice(&pdu)
                    .map_err(|_| Error::bad_database("Invalid PDU in db."))
            },
        )
    }

    #[tracing::instrument(skip(self, pdu))]
    fn add_pdu_outlier(
        &self,
        event_id: &EventId,
        pdu: &CanonicalJsonObject,
    ) -> Result<()> {
        self.eventid_outlierpdu.insert(
            event_id.as_bytes(),
            &serde_json::to_vec(&pdu).expect("CanonicalJsonObject is valid"),
        )
    }
}
