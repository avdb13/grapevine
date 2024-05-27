//! Facilities for observing runtime behavior
#![warn(missing_docs, clippy::missing_docs_in_private_items)]

use std::{fs::File, io::BufWriter};

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use tracing_flame::{FlameLayer, FlushGuard};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer, Registry};

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
