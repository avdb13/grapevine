//! Facilities for observing runtime behavior
#![warn(missing_docs, clippy::missing_docs_in_private_items)]

use std::{fs::File, io::BufWriter};

use once_cell::sync::Lazy;
use opentelemetry::{metrics::MeterProvider, KeyValue};
use opentelemetry_sdk::{metrics::SdkMeterProvider, Resource};
use tracing_flame::{FlameLayer, FlushGuard};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer, Registry};

use crate::{config::Config, error, utils::error::Result};

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

/// Initialize observability
pub(crate) fn init(config: &Config) -> Result<Guard, error::Observability> {
    let config_filter_layer = || EnvFilter::try_new(&config.log);

    let jaeger_layer = config
        .allow_jaeger
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

            Ok::<_, error::Observability>(
                telemetry.with_filter(config_filter_layer()?),
            )
        })
        .transpose()?;

    let (flame_layer, flame_guard) = config
        .tracing_flame
        .then(|| {
            let (flame_layer, guard) =
                FlameLayer::with_file("./tracing.folded")?;
            let flame_layer = flame_layer.with_empty_samples(false);

            Ok::<_, error::Observability>((
                flame_layer.with_filter(config_filter_layer()?),
                guard,
            ))
        })
        .transpose()?
        .unzip();

    let fmt_layer = tracing_subscriber::fmt::Layer::new()
        .with_filter(config_filter_layer()?);

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
}

impl Metrics {
    /// Initializes metric-collecting and exporting facilities
    fn new() -> Self {
        // Set up OpenTelemetry state
        let registry = prometheus::Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .build()
            .expect("exporter configuration should be valid");
        let provider = SdkMeterProvider::builder()
            .with_reader(exporter)
            .with_resource(standard_resource())
            .build();
        let _meter = provider.meter(env!("CARGO_PKG_NAME"));

        // TODO: Add some metrics

        Metrics {
            otel_state: (registry, provider),
        }
    }

    /// Export metrics to a string suitable for consumption by e.g. Prometheus
    pub(crate) fn export(&self) -> String {
        prometheus::TextEncoder::new()
            .encode_to_string(&self.otel_state.0.gather())
            .expect("should be able to encode metrics")
    }
}
