use std::{
    future::Future,
    io,
    net::SocketAddr,
    process::ExitCode,
    sync::{atomic, RwLock},
    time::Duration,
};

use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, MatchedPath},
    response::IntoResponse,
    routing::{any, get, on, MethodFilter},
    Router,
};
use axum_server::{
    bind, bind_rustls, tls_rustls::RustlsConfig, Handle as ServerHandle,
};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use http::{
    header::{self, HeaderName},
    Method, StatusCode, Uri,
};
use ruma::api::{
    client::{
        error::{Error as RumaError, ErrorBody, ErrorKind},
        uiaa::UiaaResponse,
    },
    IncomingRequest,
};
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::{
    cors::{self, CorsLayer},
    trace::TraceLayer,
    ServiceBuilderExt as _,
};
use tracing::{debug, info, info_span, warn, Instrument};

mod api;
mod clap;
mod config;
mod database;
mod error;
mod observability;
mod service;
mod utils;

pub(crate) use api::ruma_wrapper::{Ar, Ra};
use api::{client_server, server_server};
pub(crate) use config::Config;
pub(crate) use database::KeyValueDatabase;
pub(crate) use service::{pdu::PduEvent, Services};
#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;
pub(crate) use utils::error::{Error, Result};

#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

pub(crate) static SERVICES: RwLock<Option<&'static Services>> =
    RwLock::new(None);

/// Convenient access to the global [`Services`] instance
pub(crate) fn services() -> &'static Services {
    SERVICES
        .read()
        .unwrap()
        .expect("SERVICES should be initialized when this is called")
}

/// Returns the current version of the crate with extra info if supplied
///
/// Set the environment variable `GRAPEVINE_VERSION_EXTRA` to any UTF-8 string
/// to include it in parenthesis after the SemVer version. A common value are
/// git commit hashes.
fn version() -> String {
    let cargo_pkg_version = env!("CARGO_PKG_VERSION");

    match option_env!("GRAPEVINE_VERSION_EXTRA") {
        Some(x) => format!("{cargo_pkg_version} ({x})"),
        None => cargo_pkg_version.to_owned(),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let Err(e) = try_main().await else {
        return ExitCode::SUCCESS;
    };

    eprintln!(
        "Error: {}",
        error::DisplayWithSources {
            error: &e,
            infix: "\n    Caused by: "
        }
    );

    ExitCode::FAILURE
}

/// Fallible entrypoint
async fn try_main() -> Result<(), error::Main> {
    use error::Main as Error;

    clap::parse();

    // Initialize config
    let raw_config = Figment::new()
        .merge(
            Toml::file({
                let name = "GRAPEVINE_CONFIG";
                Env::var(name).ok_or(Error::ConfigPathUnset(name))?
            })
            .nested(),
        )
        .merge(Env::prefixed("GRAPEVINE_").global());

    let config = raw_config.extract::<Config>()?;

    config.warn_deprecated();

    let _guard = observability::init(&config);

    // This is needed for opening lots of file descriptors, which tends to
    // happen more often when using RocksDB and making lots of federation
    // connections at startup. The soft limit is usually 1024, and the hard
    // limit is usually 512000; I've personally seen it hit >2000.
    //
    // * https://www.freedesktop.org/software/systemd/man/systemd.exec.html#id-1.12.2.1.17.6
    // * https://github.com/systemd/systemd/commit/0abf94923b4a95a7d89bc526efc84e7ca2b71741
    #[cfg(unix)]
    maximize_fd_limit()
        .expect("should be able to increase the soft limit to the hard limit");

    info!("Loading database");
    KeyValueDatabase::load_or_create(config)
        .await
        .map_err(Error::DatabaseError)?;

    info!("Starting server");
    run_server().await.map_err(Error::Serve)?;

    Ok(())
}

async fn run_server() -> io::Result<()> {
    let config = &services().globals.config;
    let addr = SocketAddr::from((config.address, config.port));

    let x_requested_with = HeaderName::from_static("x-requested-with");

    let middlewares = ServiceBuilder::new()
        .sensitive_headers([header::AUTHORIZATION])
        .layer(axum::middleware::from_fn(spawn_task))
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &http::Request<_>| {
                let path = if let Some(path) =
                    request.extensions().get::<MatchedPath>()
                {
                    path.as_str()
                } else {
                    request.uri().path()
                };

                tracing::info_span!("http_request", otel.name = path, %path, method = %request.method())
            },
        ))
        .layer(axum::middleware::from_fn(unrecognized_method))
        .layer(
            CorsLayer::new()
                .allow_origin(cors::Any)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers([
                    header::ORIGIN,
                    x_requested_with,
                    header::CONTENT_TYPE,
                    header::ACCEPT,
                    header::AUTHORIZATION,
                ])
                .max_age(Duration::from_secs(86400)),
        )
        .layer(DefaultBodyLimit::max(
            config
                .max_request_size
                .try_into()
                .expect("failed to convert max request size"),
        ))
        .layer(axum::middleware::from_fn(observability::http_metrics_layer));

    let app = routes(config).layer(middlewares).into_make_service();
    let handle = ServerHandle::new();

    tokio::spawn(shutdown_signal(handle.clone()));

    match &config.tls {
        Some(tls) => {
            let conf =
                RustlsConfig::from_pem_file(&tls.certs, &tls.key).await?;
            let server = bind_rustls(addr, conf).handle(handle).serve(app);

            #[cfg(feature = "systemd")]
            sd_notify::notify(true, &[sd_notify::NotifyState::Ready])
                .expect("should be able to notify systemd");

            server.await?;
        }
        None => {
            let server = bind(addr).handle(handle).serve(app);

            #[cfg(feature = "systemd")]
            sd_notify::notify(true, &[sd_notify::NotifyState::Ready])
                .expect("should be able to notify systemd");

            server.await?;
        }
    }

    Ok(())
}

/// Ensures the request runs in a new tokio thread.
///
/// The axum request handler task gets cancelled if the connection is shut down;
/// by spawning our own task, processing continue after the client disconnects.
async fn spawn_task(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> std::result::Result<axum::response::Response, StatusCode> {
    if services().globals.shutdown.load(atomic::Ordering::Relaxed) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    tokio::spawn(next.run(req))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn unrecognized_method(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> std::result::Result<axum::response::Response, StatusCode> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let inner = next.run(req).await;
    if inner.status() == StatusCode::METHOD_NOT_ALLOWED {
        warn!("Method not allowed: {method} {uri}");
        return Ok(Ra(UiaaResponse::MatrixError(RumaError {
            body: ErrorBody::Standard {
                kind: ErrorKind::Unrecognized,
                message: "M_UNRECOGNIZED: Unrecognized request".to_owned(),
            },
            status_code: StatusCode::METHOD_NOT_ALLOWED,
        }))
        .into_response());
    }
    Ok(inner)
}

#[allow(clippy::too_many_lines)]
fn routes(config: &Config) -> Router {
    use client_server as c2s;
    use server_server as s2s;

    let router = Router::new()
        .ruma_route(c2s::get_supported_versions_route)
        .ruma_route(c2s::get_register_available_route)
        .ruma_route(c2s::register_route)
        .ruma_route(c2s::get_login_types_route)
        .ruma_route(c2s::login_route)
        .ruma_route(c2s::whoami_route)
        .ruma_route(c2s::logout_route)
        .ruma_route(c2s::logout_all_route)
        .ruma_route(c2s::change_password_route)
        .ruma_route(c2s::deactivate_route)
        .ruma_route(c2s::third_party_route)
        .ruma_route(c2s::request_3pid_management_token_via_email_route)
        .ruma_route(c2s::request_3pid_management_token_via_msisdn_route)
        .ruma_route(c2s::get_capabilities_route)
        .ruma_route(c2s::get_pushrules_all_route)
        .ruma_route(c2s::set_pushrule_route)
        .ruma_route(c2s::get_pushrule_route)
        .ruma_route(c2s::set_pushrule_enabled_route)
        .ruma_route(c2s::get_pushrule_enabled_route)
        .ruma_route(c2s::get_pushrule_actions_route)
        .ruma_route(c2s::set_pushrule_actions_route)
        .ruma_route(c2s::delete_pushrule_route)
        .ruma_route(c2s::get_room_event_route)
        .ruma_route(c2s::get_room_aliases_route)
        .ruma_route(c2s::get_filter_route)
        .ruma_route(c2s::create_filter_route)
        .ruma_route(c2s::set_global_account_data_route)
        .ruma_route(c2s::set_room_account_data_route)
        .ruma_route(c2s::get_global_account_data_route)
        .ruma_route(c2s::get_room_account_data_route)
        .ruma_route(c2s::set_displayname_route)
        .ruma_route(c2s::get_displayname_route)
        .ruma_route(c2s::set_avatar_url_route)
        .ruma_route(c2s::get_avatar_url_route)
        .ruma_route(c2s::get_profile_route)
        .ruma_route(c2s::upload_keys_route)
        .ruma_route(c2s::get_keys_route)
        .ruma_route(c2s::claim_keys_route)
        .ruma_route(c2s::create_backup_version_route)
        .ruma_route(c2s::update_backup_version_route)
        .ruma_route(c2s::delete_backup_version_route)
        .ruma_route(c2s::get_latest_backup_info_route)
        .ruma_route(c2s::get_backup_info_route)
        .ruma_route(c2s::add_backup_keys_route)
        .ruma_route(c2s::add_backup_keys_for_room_route)
        .ruma_route(c2s::add_backup_keys_for_session_route)
        .ruma_route(c2s::delete_backup_keys_for_room_route)
        .ruma_route(c2s::delete_backup_keys_for_session_route)
        .ruma_route(c2s::delete_backup_keys_route)
        .ruma_route(c2s::get_backup_keys_for_room_route)
        .ruma_route(c2s::get_backup_keys_for_session_route)
        .ruma_route(c2s::get_backup_keys_route)
        .ruma_route(c2s::set_read_marker_route)
        .ruma_route(c2s::create_receipt_route)
        .ruma_route(c2s::create_typing_event_route)
        .ruma_route(c2s::create_room_route)
        .ruma_route(c2s::redact_event_route)
        .ruma_route(c2s::report_event_route)
        .ruma_route(c2s::create_alias_route)
        .ruma_route(c2s::delete_alias_route)
        .ruma_route(c2s::get_alias_route)
        .ruma_route(c2s::join_room_by_id_route)
        .ruma_route(c2s::join_room_by_id_or_alias_route)
        .ruma_route(c2s::joined_members_route)
        .ruma_route(c2s::leave_room_route)
        .ruma_route(c2s::forget_room_route)
        .ruma_route(c2s::joined_rooms_route)
        .ruma_route(c2s::kick_user_route)
        .ruma_route(c2s::ban_user_route)
        .ruma_route(c2s::unban_user_route)
        .ruma_route(c2s::invite_user_route)
        .ruma_route(c2s::set_room_visibility_route)
        .ruma_route(c2s::get_room_visibility_route)
        .ruma_route(c2s::get_public_rooms_route)
        .ruma_route(c2s::get_public_rooms_filtered_route)
        .ruma_route(c2s::search_users_route)
        .ruma_route(c2s::get_member_events_route)
        .ruma_route(c2s::get_protocols_route)
        .ruma_route(c2s::send_message_event_route)
        .ruma_route(c2s::send_state_event_for_key_route)
        .ruma_route(c2s::get_state_events_route)
        .ruma_route(c2s::get_state_events_for_key_route)
        .ruma_route(c2s::sync_events_route)
        .ruma_route(c2s::sync_events_v4_route)
        .ruma_route(c2s::get_context_route)
        .ruma_route(c2s::get_message_events_route)
        .ruma_route(c2s::search_events_route)
        .ruma_route(c2s::turn_server_route)
        .ruma_route(c2s::send_event_to_device_route)
        .ruma_route(c2s::get_media_config_route)
        .ruma_route(c2s::create_content_route)
        .ruma_route(c2s::get_content_route)
        .ruma_route(c2s::get_content_as_filename_route)
        .ruma_route(c2s::get_content_thumbnail_route)
        .ruma_route(c2s::get_devices_route)
        .ruma_route(c2s::get_device_route)
        .ruma_route(c2s::update_device_route)
        .ruma_route(c2s::delete_device_route)
        .ruma_route(c2s::delete_devices_route)
        .ruma_route(c2s::get_tags_route)
        .ruma_route(c2s::update_tag_route)
        .ruma_route(c2s::delete_tag_route)
        .ruma_route(c2s::upload_signing_keys_route)
        .ruma_route(c2s::upload_signatures_route)
        .ruma_route(c2s::get_key_changes_route)
        .ruma_route(c2s::get_pushers_route)
        .ruma_route(c2s::set_pushers_route)
        .ruma_route(c2s::upgrade_room_route)
        .ruma_route(c2s::get_threads_route)
        .ruma_route(c2s::get_relating_events_with_rel_type_and_event_type_route)
        .ruma_route(c2s::get_relating_events_with_rel_type_route)
        .ruma_route(c2s::get_relating_events_route)
        .ruma_route(c2s::get_hierarchy_route);

    // Ruma doesn't have support for multiple paths for a single endpoint yet,
    // and these routes share one Ruma request / response type pair with
    // {get,send}_state_event_for_key_route. These two endpoints also allow
    // trailing slashes.
    let router = router
        .route(
            "/_matrix/client/r0/rooms/:room_id/state/:event_type",
            get(c2s::get_state_events_for_empty_key_route)
                .put(c2s::send_state_event_for_empty_key_route),
        )
        .route(
            "/_matrix/client/v3/rooms/:room_id/state/:event_type",
            get(c2s::get_state_events_for_empty_key_route)
                .put(c2s::send_state_event_for_empty_key_route),
        )
        .route(
            "/_matrix/client/r0/rooms/:room_id/state/:event_type/",
            get(c2s::get_state_events_for_empty_key_route)
                .put(c2s::send_state_event_for_empty_key_route),
        )
        .route(
            "/_matrix/client/v3/rooms/:room_id/state/:event_type/",
            get(c2s::get_state_events_for_empty_key_route)
                .put(c2s::send_state_event_for_empty_key_route),
        );

    let router = if config.allow_prometheus {
        router.route(
            "/metrics",
            get(|| async { observability::METRICS.export() }),
        )
    } else {
        router
    };

    let router = router
        .route(
            "/_matrix/client/r0/rooms/:room_id/initialSync",
            get(initial_sync),
        )
        .route(
            "/_matrix/client/v3/rooms/:room_id/initialSync",
            get(initial_sync),
        )
        .route("/", get(it_works))
        .fallback(not_found);

    if config.allow_federation {
        router
            .ruma_route(s2s::get_server_version_route)
            .route("/_matrix/key/v2/server", get(s2s::get_server_keys_route))
            .route(
                "/_matrix/key/v2/server/:key_id",
                get(s2s::get_server_keys_deprecated_route),
            )
            .ruma_route(s2s::get_public_rooms_route)
            .ruma_route(s2s::get_public_rooms_filtered_route)
            .ruma_route(s2s::send_transaction_message_route)
            .ruma_route(s2s::get_event_route)
            .ruma_route(s2s::get_backfill_route)
            .ruma_route(s2s::get_missing_events_route)
            .ruma_route(s2s::get_event_authorization_route)
            .ruma_route(s2s::get_room_state_route)
            .ruma_route(s2s::get_room_state_ids_route)
            .ruma_route(s2s::create_join_event_template_route)
            .ruma_route(s2s::create_join_event_v1_route)
            .ruma_route(s2s::create_join_event_v2_route)
            .ruma_route(s2s::create_invite_route)
            .ruma_route(s2s::get_devices_route)
            .ruma_route(s2s::get_room_information_route)
            .ruma_route(s2s::get_profile_information_route)
            .ruma_route(s2s::get_keys_route)
            .ruma_route(s2s::claim_keys_route)
    } else {
        router
            .route("/_matrix/federation/*path", any(federation_disabled))
            .route("/_matrix/key/*path", any(federation_disabled))
    }
}

async fn shutdown_signal(handle: ServerHandle) {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    let sig: &str;

    tokio::select! {
        () = ctrl_c => { sig = "Ctrl+C"; },
        () = terminate => { sig = "SIGTERM"; },
    }

    warn!("Received {}, shutting down...", sig);
    handle.graceful_shutdown(Some(Duration::from_secs(30)));

    services().globals.shutdown();

    #[cfg(feature = "systemd")]
    sd_notify::notify(true, &[sd_notify::NotifyState::Stopping])
        .expect("should be able to notify systemd");
}

async fn federation_disabled(_: Uri) -> impl IntoResponse {
    Error::bad_config("Federation is disabled.")
}

async fn not_found(method: Method, uri: Uri) -> impl IntoResponse {
    debug!(%method, %uri, "unknown route");
    Error::BadRequest(ErrorKind::Unrecognized, "Unrecognized request")
}

async fn initial_sync(_uri: Uri) -> impl IntoResponse {
    Error::BadRequest(
        ErrorKind::GuestAccessForbidden,
        "Guest access not implemented",
    )
}

async fn it_works() -> &'static str {
    "Hello from Grapevine!"
}

trait RouterExt {
    fn ruma_route<H, T>(self, handler: H) -> Self
    where
        H: RumaHandler<T>,
        T: 'static;
}

impl RouterExt for Router {
    fn ruma_route<H, T>(self, handler: H) -> Self
    where
        H: RumaHandler<T>,
        T: 'static,
    {
        handler.add_to_router(self)
    }
}

pub(crate) trait RumaHandler<T> {
    // Can't transform to a handler without boxing or relying on the
    // nightly-only impl-trait-in-traits feature. Moving a small amount of
    // extra logic into the trait allows bypassing both.
    fn add_to_router(self, router: Router) -> Router;
}

macro_rules! impl_ruma_handler {
    ( $($ty:ident),* $(,)? ) => {
        #[axum::async_trait]
        #[allow(non_snake_case)]
        impl<Req, Resp, E, F, Fut, $($ty,)*>
            RumaHandler<($($ty,)* Ar<Req>,)> for F
        where
            Req: IncomingRequest + Send + 'static,
            Resp: IntoResponse,
            F: FnOnce($($ty,)* Ar<Req>) -> Fut + Clone + Send + 'static,
            Fut: Future<Output = Result<Resp, E>>
                + Send,
            E: IntoResponse,
            $( $ty: FromRequestParts<()> + Send + 'static, )*
        {
            fn add_to_router(self, mut router: Router) -> Router {
                let meta = Req::METADATA;
                let method_filter = method_to_filter(meta.method);

                for path in meta.history.all_paths() {
                    let handler = self.clone();

                    router = router.route(
                        path,
                        on(
                            method_filter,
                            |$( $ty: $ty, )* req: Ar<Req>| async move {
                                let span = info_span!(
                                    "run_ruma_handler",
                                    auth.user = ?req.sender_user,
                                    auth.device = ?req.sender_device,
                                    auth.servername = ?req.sender_servername,
                                    auth.appservice_id = ?req.appservice_info
                                        .as_ref()
                                        .map(|i| &i.registration.id)
                                );
                                handler($($ty,)* req).instrument(span).await
                            }
                        )
                    )
                }

                router
            }
        }
    };
}

impl_ruma_handler!();
impl_ruma_handler!(T1);
impl_ruma_handler!(T1, T2);
impl_ruma_handler!(T1, T2, T3);
impl_ruma_handler!(T1, T2, T3, T4);
impl_ruma_handler!(T1, T2, T3, T4, T5);
impl_ruma_handler!(T1, T2, T3, T4, T5, T6);
impl_ruma_handler!(T1, T2, T3, T4, T5, T6, T7);
impl_ruma_handler!(T1, T2, T3, T4, T5, T6, T7, T8);

fn method_to_filter(method: Method) -> MethodFilter {
    match method {
        Method::DELETE => MethodFilter::DELETE,
        Method::GET => MethodFilter::GET,
        Method::HEAD => MethodFilter::HEAD,
        Method::OPTIONS => MethodFilter::OPTIONS,
        Method::PATCH => MethodFilter::PATCH,
        Method::POST => MethodFilter::POST,
        Method::PUT => MethodFilter::PUT,
        Method::TRACE => MethodFilter::TRACE,
        m => panic!("Unsupported HTTP method: {m:?}"),
    }
}

#[cfg(unix)]
#[tracing::instrument(err)]
fn maximize_fd_limit() -> Result<(), nix::errno::Errno> {
    use nix::sys::resource::{getrlimit, setrlimit, Resource};

    let res = Resource::RLIMIT_NOFILE;

    let (soft_limit, hard_limit) = getrlimit(res)?;

    debug!("Current nofile soft limit: {soft_limit}");

    setrlimit(res, hard_limit, hard_limit)?;

    debug!("Increased nofile soft limit to {hard_limit}");

    Ok(())
}
