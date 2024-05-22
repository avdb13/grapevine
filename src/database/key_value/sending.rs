use ruma::{ServerName, UserId};

use crate::{
    database::KeyValueDatabase,
    service::{
        self,
        sending::{Destination, RequestKey, SendingEventType},
    },
    services, utils, Error, Result,
};

impl service::sending::Data for KeyValueDatabase {
    fn active_requests<'a>(
        &'a self,
    ) -> Box<
        dyn Iterator<Item = Result<(RequestKey, Destination, SendingEventType)>>
            + 'a,
    > {
        Box::new(self.servercurrentevent_data.iter().map(|(key, v)| {
            let key = RequestKey::new(key);
            parse_servercurrentevent(&key, v).map(|(k, e)| (key, k, e))
        }))
    }

    fn active_requests_for<'a>(
        &'a self,
        destination: &Destination,
    ) -> Box<dyn Iterator<Item = Result<(RequestKey, SendingEventType)>> + 'a>
    {
        let prefix = destination.get_prefix();
        Box::new(self.servercurrentevent_data.scan_prefix(prefix).map(
            |(key, v)| {
                let key = RequestKey::new(key);
                parse_servercurrentevent(&key, v).map(|(_, e)| (key, e))
            },
        ))
    }

    fn delete_active_request(&self, key: RequestKey) -> Result<()> {
        self.servercurrentevent_data.remove(key.as_bytes())
    }

    fn delete_all_active_requests_for(
        &self,
        destination: &Destination,
    ) -> Result<()> {
        let prefix = destination.get_prefix();
        for (key, _) in self.servercurrentevent_data.scan_prefix(prefix) {
            self.servercurrentevent_data.remove(&key)?;
        }

        Ok(())
    }

    fn queue_requests(
        &self,
        requests: &[(&Destination, SendingEventType)],
    ) -> Result<Vec<RequestKey>> {
        let mut batch = Vec::new();
        let mut keys = Vec::new();
        for (destination, event) in requests {
            let mut key = destination.get_prefix();
            if let SendingEventType::Pdu(value) = &event {
                key.extend_from_slice(value);
            } else {
                key.extend_from_slice(
                    &services().globals.next_count()?.to_be_bytes(),
                );
            }
            let value = if let SendingEventType::Edu(value) = &event {
                &**value
            } else {
                &[]
            };
            batch.push((key.clone(), value.to_owned()));
            keys.push(RequestKey::new(key));
        }
        self.servernameevent_data.insert_batch(&mut batch.into_iter())?;
        Ok(keys)
    }

    fn queued_requests<'a>(
        &'a self,
        destination: &Destination,
    ) -> Box<dyn Iterator<Item = Result<(SendingEventType, RequestKey)>> + 'a>
    {
        let prefix = destination.get_prefix();
        return Box::new(self.servernameevent_data.scan_prefix(prefix).map(
            |(k, v)| {
                let k = RequestKey::new(k);
                parse_servercurrentevent(&k, v).map(|(_, ev)| (ev, k))
            },
        ));
    }

    fn mark_as_active(
        &self,
        events: &[(SendingEventType, RequestKey)],
    ) -> Result<()> {
        for (e, key) in events {
            let value = if let SendingEventType::Edu(value) = &e {
                &**value
            } else {
                &[]
            };
            self.servercurrentevent_data.insert(key.as_bytes(), value)?;
            self.servernameevent_data.remove(key.as_bytes())?;
        }

        Ok(())
    }

    fn set_latest_educount(
        &self,
        server_name: &ServerName,
        last_count: u64,
    ) -> Result<()> {
        self.servername_educount
            .insert(server_name.as_bytes(), &last_count.to_be_bytes())
    }

    fn get_latest_educount(&self, server_name: &ServerName) -> Result<u64> {
        self.servername_educount.get(server_name.as_bytes())?.map_or(
            Ok(0),
            |bytes| {
                utils::u64_from_bytes(&bytes).map_err(|_| {
                    Error::bad_database("Invalid u64 in servername_educount.")
                })
            },
        )
    }
}

#[tracing::instrument(skip(key, value))]
fn parse_servercurrentevent(
    key: &RequestKey,
    value: Vec<u8>,
) -> Result<(Destination, SendingEventType)> {
    let key = key.as_bytes();
    let (destination, event) = if key.starts_with(b"+") {
        let mut parts = key[1..].splitn(2, |&b| b == 0xFF);

        let server = parts.next().expect("splitn always returns one element");
        let event = parts.next().ok_or_else(|| {
            Error::bad_database("Invalid bytes in servercurrentpdus.")
        })?;

        let server = utils::string_from_bytes(server).map_err(|_| {
            Error::bad_database(
                "Invalid server bytes in server_currenttransaction",
            )
        })?;

        (Destination::Appservice(server), event)
    } else if key.starts_with(b"$") {
        let mut parts = key[1..].splitn(3, |&b| b == 0xFF);

        let user = parts.next().expect("splitn always returns one element");
        let user_string = utils::string_from_bytes(user).map_err(|_| {
            Error::bad_database("Invalid user string in servercurrentevent")
        })?;
        let user_id = UserId::parse(user_string).map_err(|_| {
            Error::bad_database("Invalid user id in servercurrentevent")
        })?;

        let pushkey = parts.next().ok_or_else(|| {
            Error::bad_database("Invalid bytes in servercurrentpdus.")
        })?;
        let pushkey_string =
            utils::string_from_bytes(pushkey).map_err(|_| {
                Error::bad_database("Invalid pushkey in servercurrentevent")
            })?;

        let event = parts.next().ok_or_else(|| {
            Error::bad_database("Invalid bytes in servercurrentpdus.")
        })?;

        (Destination::Push(user_id, pushkey_string), event)
    } else {
        let mut parts = key.splitn(2, |&b| b == 0xFF);

        let server = parts.next().expect("splitn always returns one element");
        let event = parts.next().ok_or_else(|| {
            Error::bad_database("Invalid bytes in servercurrentpdus.")
        })?;

        let server = utils::string_from_bytes(server)
            .map_err(|_| {
                Error::bad_database(
                    "Invalid server bytes in server_currenttransaction",
                )
            })?
            .try_into()
            .map_err(|_| {
                Error::bad_database(
                    "Invalid server string in server_currenttransaction",
                )
            })?;
        (Destination::Normal(server), event)
    };

    Ok((
        destination,
        if value.is_empty() {
            SendingEventType::Pdu(event.to_vec())
        } else {
            SendingEventType::Edu(value)
        },
    ))
}
