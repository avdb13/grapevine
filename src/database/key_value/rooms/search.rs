use ruma::RoomId;

use crate::{database::KeyValueDatabase, service, services, utils, Result};

/// Splits a string into tokens used as keys in the search inverted index
///
/// This may be used to tokenize both message bodies (for indexing) or search
/// queries (for querying).
fn tokenize(body: &str) -> impl Iterator<Item = String> + '_ {
    body.split_terminator(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .filter(|word| word.len() <= 50)
        .map(str::to_lowercase)
}

impl service::rooms::search::Data for KeyValueDatabase {
    #[tracing::instrument(skip(self))]
    fn index_pdu(
        &self,
        shortroomid: u64,
        pdu_id: &[u8],
        message_body: &str,
    ) -> Result<()> {
        let mut batch = tokenize(message_body).map(|word| {
            let mut key = shortroomid.to_be_bytes().to_vec();
            key.extend_from_slice(word.as_bytes());
            key.push(0xFF);
            // TODO: currently we save the room id a second time here
            key.extend_from_slice(pdu_id);
            (key, Vec::new())
        });

        self.tokenids.insert_batch(&mut batch)
    }

    #[tracing::instrument(skip(self))]
    #[allow(clippy::type_complexity)]
    fn search_pdus<'a>(
        &'a self,
        room_id: &RoomId,
        search_string: &str,
    ) -> Result<Option<(Box<dyn Iterator<Item = Vec<u8>> + 'a>, Vec<String>)>>
    {
        let prefix = services()
            .rooms
            .short
            .get_shortroomid(room_id)?
            .expect("room exists")
            .to_be_bytes()
            .to_vec();

        let words: Vec<_> = tokenize(search_string).collect();

        let iterators = words.clone().into_iter().map(move |word| {
            let mut prefix2 = prefix.clone();
            prefix2.extend_from_slice(word.as_bytes());
            prefix2.push(0xFF);
            let prefix3 = prefix2.clone();

            let mut last_possible_id = prefix2.clone();
            last_possible_id.extend_from_slice(&u64::MAX.to_be_bytes());

            self.tokenids
                // Newest pdus first
                .iter_from(&last_possible_id, true)
                .take_while(move |(k, _)| k.starts_with(&prefix2))
                .map(move |(key, _)| key[prefix3.len()..].to_vec())
        });

        // We compare b with a because we reversed the iterator earlier
        let Some(common_elements) =
            utils::common_elements(iterators, |a, b| b.cmp(a))
        else {
            return Ok(None);
        };

        Ok(Some((Box::new(common_elements), words)))
    }
}
