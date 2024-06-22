//! Facilities for observing runtime behavior
#![warn(missing_docs, clippy::missing_docs_in_private_items)]

use std::{collections::HashSet, fs::File, io::BufWriter, sync::Arc};

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use http::Method;
use once_cell::sync::Lazy;
use opentelemetry::{
    metrics::{MeterProvider, Unit},
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    metrics::{new_view, Aggregation, Instrument, SdkMeterProvider, Stream},
    Resource,
};
use strum::{AsRefStr, IntoStaticStr};
use tokio::time::Instant;
use tracing_flame::{FlameLayer, FlushGuard};
use tracing_subscriber::{
    layer::SubscriberExt, reload, EnvFilter, Layer, Registry,
};

#[allow(unused_imports)] // used in doc comments
use crate::utils::on_demand_hashmap::OnDemandHashMap;
use crate::{
    config::{Config, EnvFilterClone, LogFormat},
    error,
    utils::error::Result,
};

/// Globally accessible metrics state
pub(crate) static METRICS: Lazy<Metrics> = Lazy::new(Metrics::new);

/// Cleans up resources relating to observability when [`Drop`]ped
pub(crate) struct Guard {
    /// Drop guard used to flush [`tracing_flame`] data on exit
    #[allow(dead_code)]
    flame_guard: Option<FlushGuard<BufWriter<File>>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        opentelemetry::global::shutdown_tracer_provider();
    }
}

/// We need to store a [`reload::Handle`] value, but can't name it's type
/// explicitly because the S type parameter depends on the subscriber's previous
/// layers. In our case, this includes unnameable 'impl Trait' types.
///
/// This is fixed[1] in the unreleased tracing-subscriber from the master
/// branch, which removes the S parameter. Unfortunately can't use it without
/// pulling in a version of tracing that's incompatible with the rest of our
/// deps.
///
/// To work around this, we define an trait without the S paramter that forwards
/// to the [`reload::Handle::reload`] method, and then store the handle as a
/// trait object.
///
/// [1]: https://github.com/tokio-rs/tracing/pull/1035/commits/8a87ea52425098d3ef8f56d92358c2f6c144a28f
pub(crate) trait ReloadHandle<L> {
    /// Replace the layer with a new value. See [`reload::Handle::reload`].
    fn reload(&self, new_value: L) -> Result<(), reload::Error>;
}

impl<L, S> ReloadHandle<L> for reload::Handle<L, S> {
    fn reload(&self, new_value: L) -> Result<(), reload::Error> {
        reload::Handle::reload(self, new_value)
    }
}

/// A type-erased [reload handle][reload::Handle] for an [`EnvFilter`].
pub(crate) type FilterReloadHandle = Box<dyn ReloadHandle<EnvFilter> + Sync>;

/// Collection of [`FilterReloadHandle`]s, allowing the filters for tracing
/// backends to be changed dynamically. Handles may be [`None`] if the backend
/// is disabled in the config.
#[allow(clippy::missing_docs_in_private_items)]
pub(crate) struct FilterReloadHandles {
    pub(crate) traces: Option<FilterReloadHandle>,
    pub(crate) flame: Option<FilterReloadHandle>,
    pub(crate) log: Option<FilterReloadHandle>,
}

/// A kind of data that gets looked up
///
/// See also [`Metrics::record_lookup`].
// Keep variants sorted
#[allow(clippy::missing_docs_in_private_items)]
#[derive(Clone, Copy, AsRefStr, IntoStaticStr)]
pub(crate) enum Lookup {
    AppserviceInRoom,
    AuthChain,
    CreateEventIdToShort,
    CreateStateKeyToShort,
    FederationDestination,
    LastTimelineCount,
    OurRealUsers,
    Pdu,
    ShortToEventId,
    ShortToStateKey,
    StateInfo,
    StateKeyToShort,
    VisibilityForServer,
    VisibilityForUser,
}

/// Locations where a [`Lookup`] value may be found
///
/// Not all of these variants are used for each value of [`Lookup`].
#[derive(Clone, Copy, AsRefStr, IntoStaticStr)]
pub(crate) enum FoundIn {
    /// Found in cache
    Cache,
    /// Cache miss, but it was in the database. The cache has been updated.
    Database,
    /// Cache and database miss, but another server had it. The cache has been
    /// updated.
    Remote,
    /// The entry could not be found anywhere.
    Nothing,
}

/// Wrapper for the creation of a `tracing` [`Layer`] and any associated opaque
/// data.
///
/// Returns a no-op `None` layer if `enable` is `false`, otherwise calls the
/// given closure to construct the layer and associated data, then applies the
/// filter to the layer.
fn make_backend<S, L, T>(
    enable: bool,
    filter: &EnvFilterClone,
    init: impl FnOnce() -> Result<(L, T), error::Observability>,
) -> Result<
    (impl Layer<S>, Option<FilterReloadHandle>, Option<T>),
    error::Observability,
>
where
    L: Layer<S>,
    S: tracing::Subscriber
        + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    if !enable {
        return Ok((None, None, None));
    }

    let (filter, handle) = reload::Layer::new(EnvFilter::from(filter));
    let (layer, data) = init()?;
    Ok((Some(layer.with_filter(filter)), Some(Box::new(handle)), Some(data)))
}

/// Initialize observability
pub(crate) fn init(
    config: &Config,
) -> Result<(Guard, FilterReloadHandles), error::Observability> {
    let (traces_layer, traces_filter, _) = make_backend(
        config.observability.traces.enable,
        &config.observability.traces.filter,
        || {
            opentelemetry::global::set_text_map_propagator(
                opentelemetry_jaeger_propagator::Propagator::new(),
            );
            let mut exporter = opentelemetry_otlp::new_exporter().tonic();
            if let Some(endpoint) = &config.observability.traces.endpoint {
                exporter = exporter.with_endpoint(endpoint);
            }
            let tracer = opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_trace_config(
                    opentelemetry_sdk::trace::config()
                        .with_resource(standard_resource()),
                )
                .with_exporter(exporter)
                .install_batch(opentelemetry_sdk::runtime::Tokio)?;
            Ok((tracing_opentelemetry::layer().with_tracer(tracer), ()))
        },
    )?;

    let (flame_layer, flame_filter, flame_guard) = make_backend(
        config.observability.flame.enable,
        &config.observability.flame.filter,
        || {
            let (flame_layer, guard) =
                FlameLayer::with_file(&config.observability.flame.filename)?;
            Ok((flame_layer.with_empty_samples(false), guard))
        },
    )?;

    let (log_layer, log_filter, _) =
        make_backend(true, &config.observability.logs.filter, || {
            /// Time format selection for `tracing_subscriber` at runtime
            #[allow(clippy::missing_docs_in_private_items)]
            enum TimeFormat {
                SystemTime,
                NoTime,
            }
            impl tracing_subscriber::fmt::time::FormatTime for TimeFormat {
                fn format_time(
                    &self,
                    w: &mut tracing_subscriber::fmt::format::Writer<'_>,
                ) -> std::fmt::Result {
                    match self {
                        TimeFormat::SystemTime => {
                            tracing_subscriber::fmt::time::SystemTime
                                .format_time(w)
                        }
                        TimeFormat::NoTime => Ok(()),
                    }
                }
            }

            let fmt_layer = tracing_subscriber::fmt::Layer::new()
                .with_ansi(config.observability.logs.colors)
                .with_timer(if config.observability.logs.timestamp {
                    TimeFormat::SystemTime
                } else {
                    TimeFormat::NoTime
                });
            let fmt_layer = match config.observability.logs.format {
                LogFormat::Pretty => fmt_layer.pretty().boxed(),
                LogFormat::Full => fmt_layer.boxed(),
                LogFormat::Compact => fmt_layer.compact().boxed(),
                LogFormat::Json => fmt_layer.json().boxed(),
            };
            Ok((fmt_layer, ()))
        })?;

    let subscriber = Registry::default()
        .with(traces_layer)
        .with(flame_layer)
        .with(log_layer);
    tracing::subscriber::set_global_default(subscriber)?;

    Ok((
        Guard {
            flame_guard,
        },
        FilterReloadHandles {
            traces: traces_filter,
            flame: flame_filter,
            log: log_filter,
        },
    ))
}

/// Construct the standard [`Resource`] value to use for this service
fn standard_resource() -> Resource {
    Resource::default().merge(&Resource::new([KeyValue::new(
        "service.name",
        env!("CARGO_PKG_NAME"),
    )]))
}

/// Holds state relating to metrics
pub(crate) struct Metrics {
    /// Internal state for OpenTelemetry metrics
    ///
    /// We never directly read from [`SdkMeterProvider`], but it needs to
    /// outlive all calls to `self.otel_state.0.gather()`, otherwise
    /// metrics collection will fail.
    otel_state: (prometheus::Registry, SdkMeterProvider),

    /// Histogram of HTTP requests
    http_requests_histogram: opentelemetry::metrics::Histogram<f64>,

    /// Counts where data is found from
    lookup: opentelemetry::metrics::Counter<u64>,

    /// Number of entries in an [`OnDemandHashMap`]
    on_demand_hashmap_size: opentelemetry::metrics::Gauge<u64>,
    /// Number of times an [`OnDemandHashMap`] entry had been cloned before
    /// being dropped
    on_demand_hashmap_clone_count: opentelemetry::metrics::Histogram<u64>,
}

impl Metrics {
    /// Initializes metric-collecting and exporting facilities
    fn new() -> Self {
        // Metric names
        let http_requests_histogram_name = "http.requests";

        // Set up OpenTelemetry state
        let registry = prometheus::Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .build()
            .expect("exporter configuration should be valid");
        let provider = SdkMeterProvider::builder()
            .with_reader(exporter)
            .with_view(
                new_view(
                    Instrument::new().name(http_requests_histogram_name),
                    Stream::new().aggregation(
                        Aggregation::ExplicitBucketHistogram {
                            boundaries: vec![
                                0., 0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07,
                                0.08, 0.09, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7,
                                0.8, 0.9, 1., 2., 3., 4., 5., 6., 7., 8., 9.,
                                10., 20., 30., 40., 50.,
                            ],
                            record_min_max: true,
                        },
                    ),
                )
                .expect("view should be valid"),
            )
            .with_resource(standard_resource())
            .build();
        let meter = provider.meter(env!("CARGO_PKG_NAME"));

        // Define metrics

        let http_requests_histogram = meter
            .f64_histogram(http_requests_histogram_name)
            .with_unit(Unit::new("seconds"))
            .with_description("Histogram of HTTP requests")
            .init();

        let lookup = meter
            .u64_counter("lookup")
            .with_description("Counts where data is found from")
            .init();

        let on_demand_hashmap_size = meter
            .u64_gauge("on_demand_hashmap_size")
            .with_description("Number of entries in OnDemandHashMap")
            .init();
        let on_demand_hashmap_clone_count = meter
            .u64_histogram("on_demand_hashmap_clone_count")
            .with_description(
                "Number of times an OnDemandHashMap entry had been cloned \
                 before being dropped",
            )
            .init();

        Metrics {
            otel_state: (registry, provider),
            http_requests_histogram,
            lookup,
            on_demand_hashmap_size,
            on_demand_hashmap_clone_count,
        }
    }

    /// Export metrics to a string suitable for consumption by e.g. Prometheus
    pub(crate) fn export(&self) -> String {
        prometheus::TextEncoder::new()
            .encode_to_string(&self.otel_state.0.gather())
            .expect("should be able to encode metrics")
    }

    /// Record that some data was found in a particular storage location
    pub(crate) fn record_lookup(&self, lookup: Lookup, found_in: FoundIn) {
        self.lookup.add(
            1,
            &[
                KeyValue::new("lookup", <&str>::from(lookup)),
                KeyValue::new("found_in", <&str>::from(found_in)),
            ],
        );
    }

    /// Record verdict of cleanup performed for [`OnDemandHashMap`]
    pub(crate) fn record_on_demand_hashmap_size(
        &self,
        name: Arc<str>,
        size: usize,
    ) {
        self.on_demand_hashmap_size.record(
            size.try_into().unwrap_or(u64::MAX),
            &[KeyValue::new("name", name)],
        );
    }

    /// Record number of times an [`OnDemandHashMap`] entry had been accessed
    /// before being dropped
    pub(crate) fn record_on_demand_hashmap_clone_count(
        &self,
        name: Arc<str>,
        count: usize,
    ) {
        self.on_demand_hashmap_clone_count.record(
            count.try_into().unwrap_or(u64::MAX),
            &[KeyValue::new("name", name)],
        );
    }
}

/// Track HTTP metrics by converting this into an [`axum`] layer
pub(crate) async fn http_metrics_layer(req: Request, next: Next) -> Response {
    /// Routes that should not be included in the metrics
    static IGNORED_ROUTES: Lazy<HashSet<(&Method, &str)>> =
        Lazy::new(|| [(&Method::GET, "/metrics")].into_iter().collect());

    let matched_path =
        req.extensions().get::<MatchedPath>().map(|x| x.as_str().to_owned());

    let method = req.method().to_owned();

    match matched_path {
        // Run the next layer if the route should be ignored
        Some(matched_path)
            if IGNORED_ROUTES.contains(&(&method, matched_path.as_str())) =>
        {
            next.run(req).await
        }

        // Run the next layer if the route is unknown
        None => next.run(req).await,

        // Otherwise, run the next layer and record metrics
        Some(matched_path) => {
            let start = Instant::now();
            let resp = next.run(req).await;
            let elapsed = start.elapsed();

            let status_code = resp.status().as_str().to_owned();

            let attrs = &[
                KeyValue::new("method", method.as_str().to_owned()),
                KeyValue::new("path", matched_path),
                KeyValue::new("status_code", status_code),
            ];

            METRICS
                .http_requests_histogram
                .record(elapsed.as_secs_f64(), attrs);

            resp
        }
    }
}
