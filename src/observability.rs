//! Facilities for observing runtime behavior
#![warn(missing_docs, clippy::missing_docs_in_private_items)]

use std::{fs::File, io::BufWriter};

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use tracing_flame::{FlameLayer, FlushGuard};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};

use crate::{config::Config, error, utils::error::Result};

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
    let mut flame_guard = None;
    if config.allow_jaeger {
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_jaeger_propagator::Propagator::new(),
        );
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_trace_config(
                opentelemetry_sdk::trace::config().with_resource(
                    Resource::new(vec![KeyValue::new(
                        "service.name",
                        env!("CARGO_PKG_NAME"),
                    )]),
                ),
            )
            .with_exporter(opentelemetry_otlp::new_exporter().tonic())
            .install_batch(opentelemetry_sdk::runtime::Tokio)?;
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        let filter_layer = EnvFilter::try_new(&config.log)?;

        let subscriber = Registry::default().with(filter_layer).with(telemetry);
        tracing::subscriber::set_global_default(subscriber)?;
    } else if config.tracing_flame {
        let registry = Registry::default();
        let (flame_layer, guard) = FlameLayer::with_file("./tracing.folded")?;
        flame_guard = Some(guard);
        let flame_layer = flame_layer.with_empty_samples(false);

        let filter_layer = EnvFilter::new("trace,h2=off");

        let subscriber = registry.with(filter_layer).with(flame_layer);
        tracing::subscriber::set_global_default(subscriber)?;
    } else {
        let registry = Registry::default();
        let fmt_layer = tracing_subscriber::fmt::Layer::new();
        let filter_layer = EnvFilter::try_new(&config.log)?;

        let subscriber = registry.with(filter_layer).with(fmt_layer);
        tracing::subscriber::set_global_default(subscriber)?;
    }

    Ok(Guard {
        flame_guard,
    })
}
