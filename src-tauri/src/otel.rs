//! OpenTelemetry instrumentation for the qontinui-runner.
//!
//! Provides optional OTLP trace export via a tracing-subscriber layer.
//! When disabled (the default), no collector connection is attempted and the
//! runner behaves exactly as before.  When enabled, traces are exported over
//! gRPC (tonic) to the configured OTLP endpoint.
//!
//! # Graceful degradation
//! If initialisation fails (e.g. the collector is unreachable) the runner
//! continues without telemetry and logs a warning.

use serde::{Deserialize, Serialize};
use tracing::info;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelConfig {
    /// Master switch.  When `false` (the default) no OTel pipeline is created.
    #[serde(default)]
    pub enabled: bool,

    /// OTLP collector endpoint (default: `http://localhost:4317`).
    #[serde(default = "default_otel_endpoint")]
    pub endpoint: String,

    /// Transport protocol – currently only `"grpc"` is supported.
    #[serde(default = "default_otel_protocol")]
    pub protocol: String,

    /// Trace sampling rate in the range `[0.0, 1.0]`.
    #[serde(default = "default_sampling_rate")]
    pub sampling_rate: f64,

    /// `service.name` resource attribute.
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_otel_endpoint(),
            protocol: default_otel_protocol(),
            sampling_rate: default_sampling_rate(),
            service_name: default_service_name(),
        }
    }
}

fn default_otel_endpoint() -> String {
    "http://localhost:4317".to_string()
}
fn default_otel_protocol() -> String {
    "grpc".to_string()
}
fn default_sampling_rate() -> f64 {
    1.0
}
fn default_service_name() -> String {
    "qontinui-runner".to_string()
}

// ---------------------------------------------------------------------------
// Guard – ensures the provider is shut down when the application exits
// ---------------------------------------------------------------------------

/// Keeps the OTel [`TracerProvider`] alive and shuts it down on drop.
pub struct OtelGuard {
    _provider: Option<opentelemetry_sdk::trace::TracerProvider>,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Some(ref provider) = self._provider {
            if let Err(e) = provider.shutdown() {
                eprintln!("Error shutting down OTel provider: {:?}", e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry-point
// ---------------------------------------------------------------------------

/// Type alias for the boxed OTel layer.  Using a boxed layer avoids
/// monomorphisation issues where `OpenTelemetryLayer<Registry, Tracer>` cannot
/// satisfy `Layer<Layered<..., Registry>>`.
pub type BoxedOtelLayer = Option<
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>,
>;

/// Initialise the OpenTelemetry tracing pipeline.
///
/// Returns a guard (must be kept alive) **and** an optional boxed tracing
/// layer that should be added to the tracing subscriber **early** (before
/// other layers) via `.with(otel_layer)`.  When OTel is disabled or
/// initialisation fails the layer is `None` -- `tracing-subscriber` treats
/// `Option<Layer>` as a no-op in that case.
pub fn init_otel(config: &OtelConfig) -> (OtelGuard, BoxedOtelLayer) {
    if !config.enabled {
        info!("OpenTelemetry disabled by configuration");
        return (OtelGuard { _provider: None }, None);
    }

    match try_init_otel(config) {
        Ok((guard, layer)) => {
            info!(
                "OpenTelemetry initialized: endpoint={}, protocol={}, sampling={}",
                config.endpoint, config.protocol, config.sampling_rate
            );
            (guard, Some(Box::new(layer)))
        }
        Err(e) => {
            tracing::warn!(
                "Failed to initialize OpenTelemetry (continuing without it): {}",
                e
            );
            (OtelGuard { _provider: None }, None)
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn try_init_otel(
    config: &OtelConfig,
) -> Result<
    (
        OtelGuard,
        tracing_opentelemetry::OpenTelemetryLayer<
            tracing_subscriber::Registry,
            opentelemetry_sdk::trace::Tracer,
        >,
    ),
    Box<dyn std::error::Error>,
> {
    use opentelemetry::trace::TracerProvider;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace::{self as sdktrace, Sampler};
    use opentelemetry_sdk::Resource;

    // Build the OTLP span exporter (HTTP via reqwest)
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(&config.endpoint)
        .build()?;

    // Build sampler from the configured rate
    let sampler = if config.sampling_rate >= 1.0 {
        Sampler::AlwaysOn
    } else if config.sampling_rate <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(config.sampling_rate)
    };

    // Build the TracerProvider
    let provider = sdktrace::TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_sampler(sampler)
        .with_resource(Resource::new(vec![
            KeyValue::new("service.name", config.service_name.clone()),
            KeyValue::new(
                "service.version",
                env!("CARGO_PKG_VERSION").to_string(),
            ),
        ]))
        .build();

    let tracer = provider.tracer("qontinui-runner");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Ok((
        OtelGuard {
            _provider: Some(provider),
        },
        layer,
    ))
}
