//! Facilities for observing runtime behavior
#![warn(missing_docs, clippy::missing_docs_in_private_items)]

use std::{collections::HashSet, fs::File, io::BufWriter};

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
use opentelemetry_sdk::{
    metrics::{new_view, Aggregation, Instrument, SdkMeterProvider, Stream},
    Resource,
};
use strum::{AsRefStr, IntoStaticStr};
use tokio::time::Instant;
use tracing_flame::{FlameLayer, FlushGuard};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer, Registry};

use crate::{
    config::{Config, LogFormat},
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

/// Initialize observability
pub(crate) fn init(config: &Config) -> Result<Guard, error::Observability> {
    let jaeger_layer = config
        .observability
        .traces
        .enable
        .then(|| {
            opentelemetry::global::set_text_map_propagator(
                opentelemetry_jaeger_propagator::Propagator::new(),
            );
            let tracer = opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_trace_config(
                    opentelemetry_sdk::trace::config()
                        .with_resource(standard_resource()),
                )
                .with_exporter(opentelemetry_otlp::new_exporter().tonic())
                .install_batch(opentelemetry_sdk::runtime::Tokio)?;
            let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

            Ok::<_, error::Observability>(telemetry.with_filter(
                EnvFilter::from(&config.observability.logs.filter),
            ))
        })
        .transpose()?;

    let (flame_layer, flame_guard) = config
        .observability
        .flame
        .enable
        .then(|| {
            let (flame_layer, guard) =
                FlameLayer::with_file("./tracing.folded")?;
            let flame_layer = flame_layer.with_empty_samples(false);

            Ok::<_, error::Observability>((
                flame_layer.with_filter(EnvFilter::from(
                    &config.observability.logs.filter,
                )),
                guard,
            ))
        })
        .transpose()?
        .unzip();

    let fmt_layer = tracing_subscriber::fmt::Layer::new()
        .with_ansi(config.observability.logs.colors);
    let fmt_layer = match config.observability.logs.format {
        LogFormat::Pretty => fmt_layer.pretty().boxed(),
        LogFormat::Full => fmt_layer.boxed(),
        LogFormat::Compact => fmt_layer.compact().boxed(),
        LogFormat::Json => fmt_layer.json().boxed(),
    };
    let fmt_layer = fmt_layer
        .with_filter(EnvFilter::from(&config.observability.logs.filter));

    let subscriber = Registry::default()
        .with(jaeger_layer)
        .with(flame_layer)
        .with(fmt_layer);
    tracing::subscriber::set_global_default(subscriber)?;

    Ok(Guard {
        flame_guard,
    })
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

        Metrics {
            otel_state: (registry, provider),
            http_requests_histogram,
            lookup,
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
