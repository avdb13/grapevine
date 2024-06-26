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
use tracing::{debug, error, warn, Span};

use crate::{
    api::{appservice_server, server_server},
    services,
    utils::{calculate_hash, debug_slice_truncated},
    Config, Error, PduEvent, Result,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Destination {
    Appservice(String),
    // user and pushkey
    Push(OwnedUserId, String),
    Normal(OwnedServerName),
}

impl Destination {
    #[tracing::instrument(skip(self))]
    pub(crate) fn get_prefix(&self) -> Vec<u8> {
        let mut prefix = match self {
            Destination::Appservice(server) => {
                let mut p = b"+".to_vec();
                p.extend_from_slice(server.as_bytes());
                p
            }
            Destination::Push(user, pushkey) => {
                let mut p = b"$".to_vec();
                p.extend_from_slice(user.as_bytes());
                p.push(0xFF);
                p.extend_from_slice(pushkey.as_bytes());
                p
            }
            Destination::Normal(server) => {
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RequestKey(Vec<u8>);

impl RequestKey {
    pub(crate) fn new(key: Vec<u8>) -> Self {
        Self(key)
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub(crate) struct RequestData {
    destination: Destination,
    event_type: SendingEventType,
    key: RequestKey,
    /// Span of the original `send_*()` method call
    requester_span: Span,
}

pub(crate) struct Service {
    db: &'static dyn Data,

    /// The state for a given state hash.
    pub(super) maximum_requests: Arc<Semaphore>,
    pub(crate) sender: mpsc::UnboundedSender<RequestData>,
    receiver: Mutex<mpsc::UnboundedReceiver<RequestData>>,
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
    destination: Destination,
    events: Vec<SendingEventType>,
    /// Span of the original `send_*()` method call, if known (gets lost when
    /// event is persisted to database)
    requester_span: Option<Span>,
}

#[derive(Debug)]
struct HandlerResponse {
    destination: Destination,
    result: Result<()>,
    /// The span of the just-completed handler, for follows-from relationships.
    handler_span: Span,
}

type TransactionStatusMap = HashMap<Destination, TransactionStatus>;

enum SelectedEvents {
    None,
    Retries(Vec<SendingEventType>),
    New(Vec<SendingEventType>),
}

impl Service {
    pub(crate) fn build(db: &'static dyn Data, config: &Config) -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            db,
            sender,
            receiver: Mutex::new(receiver),
            maximum_requests: Arc::new(Semaphore::new(
                config.federation.max_concurrent_requests.into(),
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
            HashMap::<Destination, Vec<SendingEventType>>::new();

        for (key, destination, event) in
            self.db.active_requests().filter_map(Result::ok)
        {
            let entry =
                initial_transactions.entry(destination.clone()).or_default();

            if entry.len() > 30 {
                warn!(
                    "Dropping some current events: {:?} {:?} {:?}",
                    key, destination, event
                );
                self.db.delete_active_request(key)?;
                continue;
            }

            entry.push(event);
        }

        for (destination, events) in initial_transactions {
            current_transaction_status
                .insert(destination.clone(), TransactionStatus::Running);
            futures.push(handle_events(HandlerInputs {
                destination: destination.clone(),
                events,
                requester_span: None,
            }));
        }

        loop {
            select! {
                Some(response) = futures.next() => {
                    if let Some(inputs) = self.handle_response(
                        response,
                        &mut current_transaction_status,
                    )? {
                        futures.push(handle_events(inputs));
                    }
                }
                Some(data) = receiver.recv() => {
                    if let Some(inputs) = self.handle_receiver(
                        data, &mut current_transaction_status
                    ) {
                        futures.push(handle_events(inputs));
                    }
                }
            }
        }
    }

    #[tracing::instrument(
        skip(self, result, handler_span, current_transaction_status),
        fields(
            current_status = ?current_transaction_status.get(
                &destination
            ),
            error,
        ),
    )]
    fn handle_response(
        &self,
        HandlerResponse {
            destination,
            result,
            handler_span,
        }: HandlerResponse,
        current_transaction_status: &mut TransactionStatusMap,
    ) -> Result<Option<HandlerInputs>> {
        // clone() is required for the relationship to show up in jaeger
        Span::current().follows_from(handler_span.clone());

        if let Err(e) = &result {
            Span::current().record("error", e.to_string());
        }

        if let Err(error) = result {
            warn!(%error, "Marking transaction as failed");
            current_transaction_status.entry(destination).and_modify(|e| {
                use TransactionStatus::{Failed, Retrying, Running};

                *e = match e {
                    Running => Failed(1, Instant::now()),
                    Retrying(n) => Failed(*n + 1, Instant::now()),
                    Failed(..) => {
                        error!("Request that was not even running failed?!");
                        return;
                    }
                }
            });
            return Ok(None);
        }

        self.db.delete_all_active_requests_for(&destination)?;

        // Find events that have been added since starting the
        // last request
        let new_events = self
            .db
            .queued_requests(&destination)
            .filter_map(Result::ok)
            .take(30)
            .collect::<Vec<_>>();

        if new_events.is_empty() {
            current_transaction_status.remove(&destination);
            return Ok(None);
        }

        // Insert pdus we found
        self.db.mark_as_active(&new_events)?;

        Ok(Some(HandlerInputs {
            destination: destination.clone(),
            events: new_events.into_iter().map(|(event, _)| event).collect(),
            requester_span: None,
        }))
    }

    #[tracing::instrument(
        skip(self, event_type, key, requester_span, current_transaction_status),
        fields(
            current_status = ?current_transaction_status.get(&destination),
        ),
    )]
    fn handle_receiver(
        &self,
        RequestData {
            destination,
            event_type,
            key,
            requester_span,
        }: RequestData,
        current_transaction_status: &mut TransactionStatusMap,
    ) -> Option<HandlerInputs> {
        // clone() is required for the relationship to show up in jaeger
        Span::current().follows_from(requester_span.clone());

        match self.select_events(
            &destination,
            vec![(event_type, key)],
            current_transaction_status,
        ) {
            Ok(SelectedEvents::Retries(events)) => {
                debug!("retrying old events");
                Some(HandlerInputs {
                    destination,
                    events,
                    requester_span: None,
                })
            }
            Ok(SelectedEvents::New(events)) => {
                debug!("sending new event");
                Some(HandlerInputs {
                    destination,
                    events,
                    requester_span: Some(requester_span),
                })
            }
            Ok(SelectedEvents::None) => {
                debug!("holding off from sending any events");
                None
            }
            Err(error) => {
                error!(%error, "Failed to select events to send");
                None
            }
        }
    }

    #[tracing::instrument(
        skip(self, new_events, current_transaction_status),
        fields(
            new_events = debug_slice_truncated(&new_events, 3),
            current_status = ?current_transaction_status.get(destination),
        ),
    )]
    fn select_events(
        &self,
        destination: &Destination,
        // Events we want to send: event and full key
        new_events: Vec<(SendingEventType, RequestKey)>,
        current_transaction_status: &mut HashMap<
            Destination,
            TransactionStatus,
        >,
    ) -> Result<SelectedEvents> {
        let mut retry = false;
        let mut allow = true;

        let entry = current_transaction_status.entry(destination.clone());

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
            return Ok(SelectedEvents::None);
        }

        if retry {
            // We retry the previous transaction
            let events = self
                .db
                .active_requests_for(destination)
                .filter_map(Result::ok)
                .map(|(_, e)| e)
                .collect();

            Ok(SelectedEvents::Retries(events))
        } else {
            let mut events = Vec::new();

            self.db.mark_as_active(&new_events)?;
            for (e, _) in new_events {
                events.push(e);
            }

            if let Destination::Normal(server_name) = destination {
                if let Ok((select_edus, last_count)) =
                    self.select_edus(server_name)
                {
                    events.extend(
                        select_edus.into_iter().map(SendingEventType::Edu),
                    );

                    self.db.set_latest_educount(server_name, last_count)?;
                }
            }

            Ok(SelectedEvents::New(events))
        }
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
        let destination = Destination::Push(user.to_owned(), pushkey);
        let event_type = SendingEventType::Pdu(pdu_id.to_owned());
        let keys =
            self.db.queue_requests(&[(&destination, event_type.clone())])?;
        self.sender
            .send(RequestData {
                destination,
                event_type,
                key: keys.into_iter().next().unwrap(),
                requester_span: Span::current(),
            })
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
                    Destination::Normal(server),
                    SendingEventType::Pdu(pdu_id.to_owned()),
                )
            })
            .collect::<Vec<_>>();
        let keys = self.db.queue_requests(
            &requests.iter().map(|(o, e)| (o, e.clone())).collect::<Vec<_>>(),
        )?;
        for ((destination, event_type), key) in requests.into_iter().zip(keys) {
            self.sender
                .send(RequestData {
                    destination: destination.clone(),
                    event_type,
                    key,
                    requester_span: Span::current(),
                })
                .unwrap();
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
        let destination = Destination::Normal(server.to_owned());
        let event_type = SendingEventType::Edu(serialized);
        let keys =
            self.db.queue_requests(&[(&destination, event_type.clone())])?;
        self.sender
            .send(RequestData {
                destination,
                event_type,
                key: keys.into_iter().next().unwrap(),
                requester_span: Span::current(),
            })
            .unwrap();

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn send_pdu_appservice(
        &self,
        appservice_id: String,
        pdu_id: Vec<u8>,
    ) -> Result<()> {
        let destination = Destination::Appservice(appservice_id);
        let event_type = SendingEventType::Pdu(pdu_id);
        let keys =
            self.db.queue_requests(&[(&destination, event_type.clone())])?;
        self.sender
            .send(RequestData {
                destination,
                event_type,
                key: keys.into_iter().next().unwrap(),
                requester_span: Span::current(),
            })
            .unwrap();

        Ok(())
    }

    #[tracing::instrument(skip(self, request))]
    pub(crate) async fn send_federation_request<T>(
        &self,
        destination: &ServerName,
        request: T,
    ) -> Result<T::IncomingResponse>
    where
        T: OutgoingRequest + Debug,
    {
        debug!("Waiting for permit");
        let permit = self.maximum_requests.acquire().await;
        debug!("Got permit");
        let response = tokio::time::timeout(
            Duration::from_secs(2 * 60),
            server_server::send_request(destination, request, true),
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
    pub(crate) async fn send_appservice_request<T>(
        &self,
        registration: Registration,
        request: T,
    ) -> Result<Option<T::IncomingResponse>>
    where
        T: OutgoingRequest + Debug,
    {
        let permit = self.maximum_requests.acquire().await;
        let response =
            appservice_server::send_request(registration, request).await;
        drop(permit);

        response
    }
}

#[tracing::instrument(skip(events))]
async fn handle_appservice_event(
    id: &str,
    events: Vec<SendingEventType>,
) -> Result<()> {
    let mut pdu_jsons = Vec::new();

    for event in &events {
        match event {
            SendingEventType::Pdu(pdu_id) => {
                pdu_jsons.push(
                    services()
                        .rooms
                        .timeline
                        .get_pdu_from_id(pdu_id)?
                        .ok_or_else(|| {
                            Error::bad_database(
                                "[Appservice] Event in servernameevent_data \
                                 not found in db.",
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

    let permit = services().sending.maximum_requests.acquire().await;

    appservice_server::send_request(
        services().appservice.get_registration(id).await.ok_or_else(|| {
            Error::bad_database(
                "[Appservice] Could not load registration from db.",
            )
        })?,
        appservice::event::push_events::v1::Request {
            events: pdu_jsons,
            txn_id: general_purpose::URL_SAFE_NO_PAD
                .encode(calculate_hash(
                    &events
                        .iter()
                        .map(|e| match e {
                            SendingEventType::Edu(b)
                            | SendingEventType::Pdu(b) => &**b,
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
        },
    )
    .await?;

    drop(permit);

    Ok(())
}

#[tracing::instrument(skip(events))]
async fn handle_push_event(
    userid: &UserId,
    pushkey: &str,
    events: Vec<SendingEventType>,
) -> Result<()> {
    let mut pdus = Vec::new();

    for event in &events {
        match event {
            SendingEventType::Pdu(pdu_id) => {
                pdus.push(
                    services()
                        .rooms
                        .timeline
                        .get_pdu_from_id(pdu_id)?
                        .ok_or_else(|| {
                            Error::bad_database(
                                "[Push] Event in servernamevent_datas not \
                                 found in db.",
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
                serde_json::from_str::<serde_json::Value>(unsigned.get())
            {
                if unsigned.get("redacted_because").is_some() {
                    continue;
                }
            }
        }

        let Some(pusher) = services().pusher.get_pusher(userid, pushkey)?
        else {
            continue;
        };

        let rules_for_user = services()
            .account_data
            .get(
                None,
                userid,
                GlobalAccountDataEventType::PushRules.to_string().into(),
            )
            .unwrap_or_default()
            .and_then(|event| {
                serde_json::from_str::<PushRulesEvent>(event.get()).ok()
            })
            .map_or_else(
                || push::Ruleset::server_default(userid),
                |ev: PushRulesEvent| ev.content.global,
            );

        let unread: UInt = services()
            .rooms
            .user
            .notification_count(userid, &pdu.room_id)?
            .try_into()
            .expect("notification count can't go that high");

        let permit = services().sending.maximum_requests.acquire().await;

        services()
            .pusher
            .send_push_notice(userid, unread, &pusher, rules_for_user, &pdu)
            .await?;

        drop(permit);
    }

    Ok(())
}

#[tracing::instrument(skip(events))]
async fn handle_federation_event(
    server: &ServerName,
    events: Vec<SendingEventType>,
) -> Result<()> {
    let mut edu_jsons = Vec::new();
    let mut pdu_jsons = Vec::new();

    for event in &events {
        match event {
            SendingEventType::Pdu(pdu_id) => {
                // TODO: check room version and remove event_id if
                // needed
                pdu_jsons.push(PduEvent::convert_to_outgoing_federation_event(
                    services()
                        .rooms
                        .timeline
                        .get_pdu_json_from_id(pdu_id)?
                        .ok_or_else(|| {
                            error!("event not found: {server} {pdu_id:?}");
                            Error::bad_database(
                                "[Normal] Event in servernamevent_datas not \
                                 found in db.",
                            )
                        })?,
                ));
            }
            SendingEventType::Edu(edu) => {
                if let Ok(raw) = serde_json::from_slice(edu) {
                    edu_jsons.push(raw);
                }
            }
        }
    }

    let permit = services().sending.maximum_requests.acquire().await;

    let response = server_server::send_request(
        server,
        send_transaction_message::v1::Request {
            origin: services().globals.server_name().to_owned(),
            pdus: pdu_jsons,
            edus: edu_jsons,
            origin_server_ts: MilliSecondsSinceUnixEpoch::now(),
            transaction_id: general_purpose::URL_SAFE_NO_PAD
                .encode(calculate_hash(
                    &events
                        .iter()
                        .map(|e| match e {
                            SendingEventType::Edu(b)
                            | SendingEventType::Pdu(b) => &**b,
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
        },
        false,
    )
    .await?;

    for pdu in response.pdus {
        if pdu.1.is_err() {
            warn!("Failed to send to {}: {:?}", server, pdu);
        }
    }

    drop(permit);

    Ok(())
}

#[tracing::instrument(skip_all)]
async fn handle_events(
    HandlerInputs {
        destination,
        events,
        requester_span,
    }: HandlerInputs,
) -> HandlerResponse {
    if let Some(span) = requester_span {
        // clone() is required for the relationship to show up in jaeger
        Span::current().follows_from(span.clone());
    }

    let result = match &destination {
        Destination::Appservice(id) => {
            handle_appservice_event(id, events).await
        }
        Destination::Push(userid, pushkey) => {
            handle_push_event(userid, pushkey, events).await
        }
        Destination::Normal(server) => {
            handle_federation_event(server, events).await
        }
    };

    HandlerResponse {
        destination,
        result,
        handler_span: Span::current(),
    }
}
