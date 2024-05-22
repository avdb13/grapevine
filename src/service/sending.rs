mod data;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Debug,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose, Engine as _};
pub(crate) use data::Data;
use federation::transactions::send_transaction_message;
use futures_util::{stream::FuturesUnordered, StreamExt};
use ruma::{
    api::{
        appservice::{self, Registration},
        federation::{
            self,
            transactions::edu::{
                DeviceListUpdateContent, Edu, ReceiptContent, ReceiptData,
                ReceiptMap,
            },
        },
        OutgoingRequest,
    },
    device_id,
    events::{
        push_rules::PushRulesEvent, receipt::ReceiptType,
        AnySyncEphemeralRoomEvent, GlobalAccountDataEventType,
    },
    push, uint, MilliSecondsSinceUnixEpoch, OwnedServerName, OwnedUserId,
    ServerName, UInt, UserId,
};
use tokio::{
    select,
    sync::{mpsc, Mutex, Semaphore},
};
use tracing::{debug, error, warn};

use crate::{
    api::{appservice_server, server_server},
    services,
    utils::{calculate_hash, debug_slice_truncated},
    Config, Error, PduEvent, Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum OutgoingKind {
    Appservice(String),
    // user and pushkey
    Push(OwnedUserId, String),
    Normal(OwnedServerName),
}

impl OutgoingKind {
    #[tracing::instrument(skip(self))]
    pub(crate) fn get_prefix(&self) -> Vec<u8> {
        let mut prefix = match self {
            OutgoingKind::Appservice(server) => {
                let mut p = b"+".to_vec();
                p.extend_from_slice(server.as_bytes());
                p
            }
            OutgoingKind::Push(user, pushkey) => {
                let mut p = b"$".to_vec();
                p.extend_from_slice(user.as_bytes());
                p.push(0xFF);
                p.extend_from_slice(pushkey.as_bytes());
                p
            }
            OutgoingKind::Normal(server) => {
                let mut p = Vec::new();
                p.extend_from_slice(server.as_bytes());
                p
            }
        };
        prefix.push(0xFF);

        prefix
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SendingEventType {
    // pduid
    Pdu(Vec<u8>),
    // pdu json
    Edu(Vec<u8>),
}

pub(crate) struct Service {
    db: &'static dyn Data,

    /// The state for a given state hash.
    pub(super) maximum_requests: Arc<Semaphore>,
    pub(crate) sender:
        mpsc::UnboundedSender<(OutgoingKind, SendingEventType, Vec<u8>)>,
    receiver: Mutex<
        mpsc::UnboundedReceiver<(OutgoingKind, SendingEventType, Vec<u8>)>,
    >,
}

#[derive(Debug)]
enum TransactionStatus {
    Running,
    // number of times failed, time of last failure
    Failed(u32, Instant),
    // number of times failed
    Retrying(u32),
}

struct HandlerInputs {
    kind: OutgoingKind,
    events: Vec<SendingEventType>,
}
type HandlerResponse = Result<OutgoingKind, (OutgoingKind, Error)>;

fn outgoing_kind_from_response(response: &HandlerResponse) -> &OutgoingKind {
    match response {
        Ok(kind) | Err((kind, _)) => kind,
    }
}

type TransactionStatusMap = HashMap<OutgoingKind, TransactionStatus>;

impl Service {
    pub(crate) fn build(db: &'static dyn Data, config: &Config) -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            db,
            sender,
            receiver: Mutex::new(receiver),
            maximum_requests: Arc::new(Semaphore::new(
                config.max_concurrent_requests.into(),
            )),
        })
    }

    pub(crate) fn start_handler(self: &Arc<Self>) {
        let self2 = Arc::clone(self);
        tokio::spawn(async move {
            self2.handler().await.unwrap();
        });
    }

    async fn handler(&self) -> Result<()> {
        let mut receiver = self.receiver.lock().await;

        let mut futures = FuturesUnordered::new();

        let mut current_transaction_status = TransactionStatusMap::new();

        // Retry requests we could not finish yet
        let mut initial_transactions =
            HashMap::<OutgoingKind, Vec<SendingEventType>>::new();

        for (key, outgoing_kind, event) in
            self.db.active_requests().filter_map(Result::ok)
        {
            let entry =
                initial_transactions.entry(outgoing_kind.clone()).or_default();

            if entry.len() > 30 {
                warn!(
                    "Dropping some current events: {:?} {:?} {:?}",
                    key, outgoing_kind, event
                );
                self.db.delete_active_request(key)?;
                continue;
            }

            entry.push(event);
        }

        for (outgoing_kind, events) in initial_transactions {
            current_transaction_status
                .insert(outgoing_kind.clone(), TransactionStatus::Running);
            futures.push(Self::handle_events(outgoing_kind.clone(), events));
        }

        loop {
            select! {
                Some(response) = futures.next() =>
                    if let Some(HandlerInputs { kind, events }) =
                        self.handle_futures(
                            response,
                            &mut current_transaction_status,
                        )?
                    {
                        futures.push(Self::handle_events(kind, events));
                    },
                Some((outgoing_kind, event, key)) = receiver.recv() =>
                    if let Some(HandlerInputs { kind, events }) =
                        self.handle_receiver(
                            outgoing_kind,
                            event,
                            key,
                            &mut current_transaction_status,
                        )
                    {
                        futures.push(Self::handle_events(kind, events));
                    }
            }
        }
    }

    #[tracing::instrument(
        skip(self, current_transaction_status),
        fields(
            current_status = ?current_transaction_status.get(
                outgoing_kind_from_response(&response)
            ),
        ),
    )]
    fn handle_futures(
        &self,
        response: HandlerResponse,
        current_transaction_status: &mut TransactionStatusMap,
    ) -> Result<Option<HandlerInputs>> {
        match response {
            Ok(outgoing_kind) => {
                self.db.delete_all_active_requests_for(&outgoing_kind)?;

                // Find events that have been added since starting the
                // last request
                let new_events = self
                    .db
                    .queued_requests(&outgoing_kind)
                    .filter_map(Result::ok)
                    .take(30)
                    .collect::<Vec<_>>();

                if new_events.is_empty() {
                    current_transaction_status.remove(&outgoing_kind);
                    Ok(None)
                } else {
                    // Insert pdus we found
                    self.db.mark_as_active(&new_events)?;

                    Ok(Some(HandlerInputs {
                        kind: outgoing_kind.clone(),
                        events: new_events
                            .into_iter()
                            .map(|(event, _)| event)
                            .collect(),
                    }))
                }
            }
            Err((outgoing_kind, _)) => {
                current_transaction_status.entry(outgoing_kind).and_modify(
                    |e| {
                        *e = match e {
                            TransactionStatus::Running => {
                                TransactionStatus::Failed(1, Instant::now())
                            }
                            TransactionStatus::Retrying(n) => {
                                TransactionStatus::Failed(
                                    *n + 1,
                                    Instant::now(),
                                )
                            }
                            TransactionStatus::Failed(..) => {
                                error!(
                                    "Request that was not even running \
                                     failed?!"
                                );
                                return;
                            }
                        }
                    },
                );
                Ok(None)
            }
        }
    }

    #[tracing::instrument(
        skip(self, event, key, current_transaction_status),
        fields(
            current_status = ?current_transaction_status.get(&outgoing_kind),
        ),
    )]
    fn handle_receiver(
        &self,
        outgoing_kind: OutgoingKind,
        event: SendingEventType,
        key: Vec<u8>,
        current_transaction_status: &mut TransactionStatusMap,
    ) -> Option<HandlerInputs> {
        if let Ok(Some(events)) = self.select_events(
            &outgoing_kind,
            vec![(event, key)],
            current_transaction_status,
        ) {
            Some(HandlerInputs {
                kind: outgoing_kind,
                events,
            })
        } else {
            None
        }
    }

    #[tracing::instrument(
        skip(self, new_events, current_transaction_status),
        fields(
            new_events = debug_slice_truncated(&new_events, 3),
            current_status = ?current_transaction_status.get(outgoing_kind),
        ),
    )]
    fn select_events(
        &self,
        outgoing_kind: &OutgoingKind,
        // Events we want to send: event and full key
        new_events: Vec<(SendingEventType, Vec<u8>)>,
        current_transaction_status: &mut HashMap<
            OutgoingKind,
            TransactionStatus,
        >,
    ) -> Result<Option<Vec<SendingEventType>>> {
        let mut retry = false;
        let mut allow = true;

        let entry = current_transaction_status.entry(outgoing_kind.clone());

        entry
            .and_modify(|e| match e {
                TransactionStatus::Running | TransactionStatus::Retrying(_) => {
                    // already running
                    allow = false;
                }
                TransactionStatus::Failed(tries, time) => {
                    // Fail if a request has failed recently (exponential
                    // backoff)
                    let mut min_elapsed_duration =
                        Duration::from_secs(30) * (*tries) * (*tries);
                    if min_elapsed_duration > Duration::from_secs(60 * 60 * 24)
                    {
                        min_elapsed_duration =
                            Duration::from_secs(60 * 60 * 24);
                    }

                    if time.elapsed() < min_elapsed_duration {
                        allow = false;
                    } else {
                        retry = true;
                        *e = TransactionStatus::Retrying(*tries);
                    }
                }
            })
            .or_insert(TransactionStatus::Running);

        if !allow {
            return Ok(None);
        }

        let mut events = Vec::new();

        if retry {
            // We retry the previous transaction
            for (_, e) in self
                .db
                .active_requests_for(outgoing_kind)
                .filter_map(Result::ok)
            {
                events.push(e);
            }
        } else {
            self.db.mark_as_active(&new_events)?;
            for (e, _) in new_events {
                events.push(e);
            }

            if let OutgoingKind::Normal(server_name) = outgoing_kind {
                if let Ok((select_edus, last_count)) =
                    self.select_edus(server_name)
                {
                    events.extend(
                        select_edus.into_iter().map(SendingEventType::Edu),
                    );

                    self.db.set_latest_educount(server_name, last_count)?;
                }
            }
        }

        Ok(Some(events))
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn select_edus(
        &self,
        server_name: &ServerName,
    ) -> Result<(Vec<Vec<u8>>, u64)> {
        // u64: count of last edu
        let since = self.db.get_latest_educount(server_name)?;
        let mut events = Vec::new();
        let mut max_edu_count = since;
        let mut device_list_changes = HashSet::new();

        'outer: for room_id in
            services().rooms.state_cache.server_rooms(server_name)
        {
            let room_id = room_id?;
            // Look for device list updates in this room
            device_list_changes.extend(
                services()
                    .users
                    .keys_changed(room_id.as_ref(), since, None)
                    .filter_map(Result::ok)
                    .filter(|user_id| {
                        user_id.server_name()
                            == services().globals.server_name()
                    }),
            );

            // Look for read receipts in this room
            for r in services()
                .rooms
                .edus
                .read_receipt
                .readreceipts_since(&room_id, since)
            {
                let (user_id, count, read_receipt) = r?;

                if count > max_edu_count {
                    max_edu_count = count;
                }

                if user_id.server_name() != services().globals.server_name() {
                    continue;
                }

                let event: AnySyncEphemeralRoomEvent = serde_json::from_str(
                    read_receipt.json().get(),
                )
                .map_err(|_| {
                    Error::bad_database("Invalid edu event in read_receipts.")
                })?;
                let federation_event =
                    if let AnySyncEphemeralRoomEvent::Receipt(r) = event {
                        let mut read = BTreeMap::new();

                        let (event_id, mut receipt) =
                            r.content.0.into_iter().next().expect(
                                "we only use one event per read receipt",
                            );
                        let receipt = receipt
                            .remove(&ReceiptType::Read)
                            .expect("our read receipts always set this")
                            .remove(&user_id)
                            .expect(
                                "our read receipts always have the user here",
                            );

                        read.insert(
                            user_id,
                            ReceiptData {
                                data: receipt.clone(),
                                event_ids: vec![event_id.clone()],
                            },
                        );

                        let receipt_map = ReceiptMap {
                            read,
                        };

                        let mut receipts = BTreeMap::new();
                        receipts.insert(room_id.clone(), receipt_map);

                        Edu::Receipt(ReceiptContent {
                            receipts,
                        })
                    } else {
                        Error::bad_database(
                            "Invalid event type in read_receipts",
                        );
                        continue;
                    };

                events.push(
                    serde_json::to_vec(&federation_event)
                        .expect("json can be serialized"),
                );

                if events.len() >= 20 {
                    break 'outer;
                }
            }
        }

        for user_id in device_list_changes {
            // Empty prev id forces synapse to resync: https://github.com/matrix-org/synapse/blob/98aec1cc9da2bd6b8e34ffb282c85abf9b8b42ca/synapse/handlers/device.py#L767
            // Because synapse resyncs, we can just insert dummy data
            let edu = Edu::DeviceListUpdate(DeviceListUpdateContent {
                user_id,
                device_id: device_id!("dummy").to_owned(),
                device_display_name: Some("Dummy".to_owned()),
                stream_id: uint!(1),
                prev_id: Vec::new(),
                deleted: None,
                keys: None,
            });

            events.push(
                serde_json::to_vec(&edu).expect("json can be serialized"),
            );
        }

        Ok((events, max_edu_count))
    }

    #[tracing::instrument(skip(self, pdu_id, user, pushkey))]
    pub(crate) fn send_push_pdu(
        &self,
        pdu_id: &[u8],
        user: &UserId,
        pushkey: String,
    ) -> Result<()> {
        let outgoing_kind = OutgoingKind::Push(user.to_owned(), pushkey);
        let event = SendingEventType::Pdu(pdu_id.to_owned());
        let keys =
            self.db.queue_requests(&[(&outgoing_kind, event.clone())])?;
        self.sender
            .send((outgoing_kind, event, keys.into_iter().next().unwrap()))
            .unwrap();

        Ok(())
    }

    #[tracing::instrument(skip(self, servers, pdu_id))]
    pub(crate) fn send_pdu<I: Iterator<Item = OwnedServerName>>(
        &self,
        servers: I,
        pdu_id: &[u8],
    ) -> Result<()> {
        let requests = servers
            .into_iter()
            .map(|server| {
                (
                    OutgoingKind::Normal(server),
                    SendingEventType::Pdu(pdu_id.to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let keys = self.db.queue_requests(
            &requests.iter().map(|(o, e)| (o, e.clone())).collect::<Vec<_>>(),
        )?;
        for ((outgoing_kind, event), key) in requests.into_iter().zip(keys) {
            self.sender.send((outgoing_kind.clone(), event, key)).unwrap();
        }

        Ok(())
    }

    #[tracing::instrument(skip(self, server, serialized))]
    pub(crate) fn send_reliable_edu(
        &self,
        server: &ServerName,
        serialized: Vec<u8>,
        id: u64,
    ) -> Result<()> {
        let outgoing_kind = OutgoingKind::Normal(server.to_owned());
        let event = SendingEventType::Edu(serialized);
        let keys =
            self.db.queue_requests(&[(&outgoing_kind, event.clone())])?;
        self.sender
            .send((outgoing_kind, event, keys.into_iter().next().unwrap()))
            .unwrap();

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn send_pdu_appservice(
        &self,
        appservice_id: String,
        pdu_id: Vec<u8>,
    ) -> Result<()> {
        let outgoing_kind = OutgoingKind::Appservice(appservice_id);
        let event = SendingEventType::Pdu(pdu_id);
        let keys =
            self.db.queue_requests(&[(&outgoing_kind, event.clone())])?;
        self.sender
            .send((outgoing_kind, event, keys.into_iter().next().unwrap()))
            .unwrap();

        Ok(())
    }

    #[tracing::instrument(skip(events))]
    async fn handle_events(
        kind: OutgoingKind,
        events: Vec<SendingEventType>,
    ) -> HandlerResponse {
        match &kind {
            OutgoingKind::Appservice(id) => {
                let mut pdu_jsons = Vec::new();

                for event in &events {
                    match event {
                        SendingEventType::Pdu(pdu_id) => {
                            pdu_jsons.push(
                                services()
                                    .rooms
                                    .timeline
                                    .get_pdu_from_id(pdu_id)
                                    .map_err(|e| (kind.clone(), e))?
                                    .ok_or_else(|| {
                                        (
                                            kind.clone(),
                                            Error::bad_database(
                                                "[Appservice] Event in \
                                                 servernameevent_data not \
                                                 found in db.",
                                            ),
                                        )
                                    })?
                                    .to_room_event(),
                            );
                        }
                        SendingEventType::Edu(_) => {
                            // Appservices don't need EDUs (?)
                        }
                    }
                }

                let permit =
                    services().sending.maximum_requests.acquire().await;

                let response = match appservice_server::send_request(
                    services()
                        .appservice
                        .get_registration(id)
                        .await
                        .ok_or_else(|| {
                            (
                                kind.clone(),
                                Error::bad_database(
                                    "[Appservice] Could not load registration \
                                     from db.",
                                ),
                            )
                        })?,
                    appservice::event::push_events::v1::Request {
                        events: pdu_jsons,
                        txn_id: (&*general_purpose::URL_SAFE_NO_PAD.encode(
                            calculate_hash(
                                &events
                                    .iter()
                                    .map(|e| match e {
                                        SendingEventType::Edu(b)
                                        | SendingEventType::Pdu(b) => &**b,
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                        ))
                            .into(),
                    },
                )
                .await
                {
                    Ok(_) => Ok(kind.clone()),
                    Err(e) => Err((kind.clone(), e)),
                };

                drop(permit);

                response
            }
            OutgoingKind::Push(userid, pushkey) => {
                let mut pdus = Vec::new();

                for event in &events {
                    match event {
                        SendingEventType::Pdu(pdu_id) => {
                            pdus.push(
                                services()
                                    .rooms
                                    .timeline
                                    .get_pdu_from_id(pdu_id)
                                    .map_err(|e| (kind.clone(), e))?
                                    .ok_or_else(|| {
                                        (
                                            kind.clone(),
                                            Error::bad_database(
                                                "[Push] Event in \
                                                 servernamevent_datas not \
                                                 found in db.",
                                            ),
                                        )
                                    })?,
                            );
                        }
                        // Push gateways don't need EDUs (?)
                        SendingEventType::Edu(_) => {}
                    }
                }

                for pdu in pdus {
                    // Redacted events are not notification targets (we don't
                    // send push for them)
                    if let Some(unsigned) = &pdu.unsigned {
                        if let Ok(unsigned) =
                            serde_json::from_str::<serde_json::Value>(
                                unsigned.get(),
                            )
                        {
                            if unsigned.get("redacted_because").is_some() {
                                continue;
                            }
                        }
                    }

                    let Some(pusher) = services()
                        .pusher
                        .get_pusher(userid, pushkey)
                        .map_err(|e| {
                            (
                                OutgoingKind::Push(
                                    userid.clone(),
                                    pushkey.clone(),
                                ),
                                e,
                            )
                        })?
                    else {
                        continue;
                    };

                    let rules_for_user = services()
                        .account_data
                        .get(
                            None,
                            userid,
                            GlobalAccountDataEventType::PushRules
                                .to_string()
                                .into(),
                        )
                        .unwrap_or_default()
                        .and_then(|event| {
                            serde_json::from_str::<PushRulesEvent>(event.get())
                                .ok()
                        })
                        .map_or_else(
                            || push::Ruleset::server_default(userid),
                            |ev: PushRulesEvent| ev.content.global,
                        );

                    let unread: UInt = services()
                        .rooms
                        .user
                        .notification_count(userid, &pdu.room_id)
                        .map_err(|e| (kind.clone(), e))?
                        .try_into()
                        .expect("notification count can't go that high");

                    let permit =
                        services().sending.maximum_requests.acquire().await;

                    let _response = services()
                        .pusher
                        .send_push_notice(
                            userid,
                            unread,
                            &pusher,
                            rules_for_user,
                            &pdu,
                        )
                        .await
                        .map(|_response| kind.clone())
                        .map_err(|e| (kind.clone(), e));

                    drop(permit);
                }
                Ok(OutgoingKind::Push(userid.clone(), pushkey.clone()))
            }
            OutgoingKind::Normal(server) => {
                let mut edu_jsons = Vec::new();
                let mut pdu_jsons = Vec::new();

                for event in &events {
                    match event {
                        SendingEventType::Pdu(pdu_id) => {
                            // TODO: check room version and remove event_id if
                            // needed
                            let raw =
                                PduEvent::convert_to_outgoing_federation_event(
                                    services()
                                        .rooms
                                        .timeline
                                        .get_pdu_json_from_id(pdu_id)
                                        .map_err(|e| {
                                            (
                                                OutgoingKind::Normal(
                                                    server.clone(),
                                                ),
                                                e,
                                            )
                                        })?
                                        .ok_or_else(|| {
                                            error!(
                                                "event not found: {server} \
                                                 {pdu_id:?}"
                                            );
                                            (
                                                OutgoingKind::Normal(
                                                    server.clone(),
                                                ),
                                                Error::bad_database(
                                                    "[Normal] Event in \
                                                     servernamevent_datas not \
                                                     found in db.",
                                                ),
                                            )
                                        })?,
                                );
                            pdu_jsons.push(raw);
                        }
                        SendingEventType::Edu(edu) => {
                            if let Ok(raw) = serde_json::from_slice(edu) {
                                edu_jsons.push(raw);
                            }
                        }
                    }
                }

                let permit =
                    services().sending.maximum_requests.acquire().await;

                let response = server_server::send_request(
                    server,
                    send_transaction_message::v1::Request {
                        origin: services().globals.server_name().to_owned(),
                        pdus: pdu_jsons,
                        edus: edu_jsons,
                        origin_server_ts: MilliSecondsSinceUnixEpoch::now(),
                        transaction_id: (&*general_purpose::URL_SAFE_NO_PAD
                            .encode(calculate_hash(
                                &events
                                    .iter()
                                    .map(|e| match e {
                                        SendingEventType::Edu(b)
                                        | SendingEventType::Pdu(b) => &**b,
                                    })
                                    .collect::<Vec<_>>(),
                            )))
                            .into(),
                    },
                )
                .await
                .map(|response| {
                    for pdu in response.pdus {
                        if pdu.1.is_err() {
                            warn!("Failed to send to {}: {:?}", server, pdu);
                        }
                    }
                    kind.clone()
                })
                .map_err(|e| (kind, e));

                drop(permit);

                response
            }
        }
    }

    #[tracing::instrument(skip(self, request))]
    pub(crate) async fn send_federation_request<T: OutgoingRequest>(
        &self,
        destination: &ServerName,
        request: T,
    ) -> Result<T::IncomingResponse>
    where
        T: Debug,
    {
        debug!("Waiting for permit");
        let permit = self.maximum_requests.acquire().await;
        debug!("Got permit");
        let response = tokio::time::timeout(
            Duration::from_secs(2 * 60),
            server_server::send_request(destination, request),
        )
        .await
        .map_err(|_| {
            warn!("Timeout waiting for server response of {destination}");
            Error::BadServerResponse("Timeout waiting for server response")
        })?;
        drop(permit);

        response
    }

    /// Sends a request to an appservice
    ///
    /// Only returns None if there is no url specified in the appservice
    /// registration file
    #[tracing::instrument(
        skip(self, registration, request),
        fields(appservice_id = registration.id),
    )]
    pub(crate) async fn send_appservice_request<T: OutgoingRequest>(
        &self,
        registration: Registration,
        request: T,
    ) -> Result<Option<T::IncomingResponse>>
    where
        T: Debug,
    {
        let permit = self.maximum_requests.acquire().await;
        let response =
            appservice_server::send_request(registration, request).await;
        drop(permit);

        response
    }
}
