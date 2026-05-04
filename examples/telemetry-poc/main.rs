// Copyright 2026 The Drasi Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");

//! POC verifying the drasi-server observability design.
//!
//! What this validates from the design doc:
//!
//! 1. The `telemetry` YAML config (with env-var interpolation) round-trips into
//!    a `TelemetryConfig` struct.
//! 2. A multi-layer `tracing::Subscriber` composes:
//!       - drasi-lib's `ComponentLogLayer` (here: a stand-in that captures
//!         events to an in-memory sink, mimicking drasi-lib's per-component
//!         log streaming layer)
//!       - a `fmt` layer for stdout
//!       - an OTLP layer (here: backed by `opentelemetry-stdout` + a custom
//!         in-memory `SpanExporter` so we can assert spans flowed to it,
//!         instead of needing a real Jaeger/Tempo collector)
//! 3. A `metrics::Recorder` is installed:
//!       - Prometheus scrape endpoint exposed on a configurable port
//!       - drasi-lib-style metric calls land at `/metrics`
//! 4. With the `telemetry` section omitted, only the component layer + fmt
//!    layer are active (no external connections, no recorder installed) —
//!    matching the "default config" scenario (User Scenario 1) in the design.
//! 5. Graceful shutdown flushes pending spans before exit
//!    (`tracer_provider.shutdown()`).
//!
//! What this intentionally does NOT do:
//!   - Wire in real Axum routes (out of scope per the design's Non-Goals).
//!   - Connect to a real OTLP collector — we use an in-memory exporter so the
//!     POC is hermetic and self-asserting.
//!   - Use the real drasi-lib `ComponentLogLayer` — drasi-lib has not yet
//!     split `init_component_log_layer()` out (open issue in the drasi-lib
//!     design); we model that future API surface here.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_sdk::export::trace::{ExportResult, SpanData, SpanExporter};
use opentelemetry_sdk::trace as sdktrace;
use opentelemetry_sdk::Resource;
use serde::Deserialize;
use tracing::{field::Visit, info, info_span, Subscriber};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

// =====================================================================
//  1. CONFIG TYPES — mirrors the YAML shape proposed in the design doc.
// =====================================================================

#[derive(Debug, Clone, Deserialize, Default)]
struct DrasiServerConfig {
    #[serde(default = "default_log_level")]
    log_level: String,
    #[serde(default)]
    telemetry: Option<TelemetryConfig>,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
struct TelemetryConfig {
    #[serde(default)]
    tracing: Option<TracingConfig>,
    #[serde(default)]
    metrics: Option<MetricsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct TracingConfig {
    /// OTLP endpoint. Empty / absent disables OTLP export.
    #[serde(default)]
    endpoint: String,
    #[serde(default = "default_service_name")]
    service_name: String,
}

fn default_service_name() -> String {
    "drasi-server".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
enum MetricsConfig {
    Prometheus(PrometheusConfig),
    Otlp(OtlpMetricsConfig),
}

#[derive(Debug, Clone, Deserialize)]
struct PrometheusConfig {
    port: u16,
    #[serde(default = "default_prom_path")]
    path: String,
}

fn default_prom_path() -> String {
    "/metrics".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // path used in real impl; not exercised in this hermetic POC.
struct OtlpMetricsConfig {
    endpoint: String,
    export_interval: u64,
}

// =====================================================================
//  2. STAND-IN FOR drasi-lib::init_component_log_layer()
//     (Per Open Issue #5 in the drasi-lib design — drasi-lib will expose a
//      Layer, and drasi-server will compose it into its own subscriber.)
// =====================================================================

/// Sink the (mock) ComponentLogLayer writes into. In the real drasi-lib this
/// is per-component channels exposed via the `/api/components/{id}/logs`
/// streaming endpoint; here we just collect into a Vec for assertion.
#[derive(Default, Debug)]
struct ComponentLogSink {
    events: Mutex<Vec<ComponentLogEvent>>,
}

#[derive(Debug, Clone)]
struct ComponentLogEvent {
    target: String,
    message: String,
}

/// A `tracing::Layer` that captures every event into the sink. This mimics
/// what drasi-lib's real `ComponentLogLayer` does (route events to per-
/// component streams) — we just keep it simple and append to a Vec.
struct ComponentLogLayer {
    sink: Arc<ComponentLogSink>,
}

impl<S> Layer<S> for ComponentLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct MsgVisitor(Option<String>);
        impl Visit for MsgVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = Some(format!("{:?}", value));
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.0 = Some(value.to_string());
                }
            }
        }
        let mut v = MsgVisitor(None);
        event.record(&mut v);
        self.sink.events.lock().unwrap().push(ComponentLogEvent {
            target: event.metadata().target().to_string(),
            message: v.0.unwrap_or_default(),
        });
    }
}

fn init_component_log_layer() -> (Arc<ComponentLogSink>, ComponentLogLayer) {
    let sink = Arc::new(ComponentLogSink::default());
    let layer = ComponentLogLayer { sink: sink.clone() };
    (sink, layer)
}

// =====================================================================
//  3. OTLP EXPORTER STAND-INS
//     A real deployment would point at jaeger:4317 via `opentelemetry-otlp`.
//     We provide three flavors so we can exercise the failure / flush paths
//     deterministically:
//       - InMemory: collects on `export` (used by the basic happy-path).
//       - Broken:   always returns `Err` from `export` — simulates an
//                   unreachable OTLP endpoint (Verification table row #7).
//       - Buffering: collects on `export`, but is paired with a
//                   BatchSpanProcessor so spans are *not* flushed until
//                   `force_flush` is called (Verification table row #6).
// =====================================================================

#[derive(Debug, Clone, Default)]
struct OtlpSpanSink {
    spans: Arc<Mutex<Vec<SpanData>>>,
}

#[derive(Debug)]
struct InMemoryOtlpExporter {
    sink: OtlpSpanSink,
}

impl SpanExporter for InMemoryOtlpExporter {
    fn export(&mut self, batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        let sink = self.sink.spans.clone();
        Box::pin(async move {
            sink.lock().unwrap().extend(batch);
            Ok(())
        })
    }
}

/// Exporter that always errors. Used to verify the server starts and runs
/// gracefully when the OTLP endpoint is unreachable.
#[derive(Debug, Default)]
struct BrokenOtlpExporter {
    /// Number of times `export` was called (and failed) — used for assertion.
    error_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl SpanExporter for BrokenOtlpExporter {
    fn export(&mut self, _batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        self.error_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            Err(opentelemetry::trace::TraceError::Other(
                "simulated OTLP unreachable".into(),
            ))
        })
    }
}

/// Choice of OTLP backend wiring. Lets each scenario plug in the exporter +
/// processor combo it needs without forking `init_tracing`.
enum OtlpBackend {
    /// `SimpleSpanProcessor` + InMemoryOtlpExporter — span-end exports.
    InMemorySimple,
    /// `SimpleSpanProcessor` + BrokenOtlpExporter — every export fails.
    BrokenSimple {
        error_count: Arc<std::sync::atomic::AtomicUsize>,
    },
    /// `BatchSpanProcessor` + InMemoryOtlpExporter — spans buffered until flush.
    InMemoryBatch,
}

// =====================================================================
//  3.5 ENV-VAR INTERPOLATION (verifies the documented `${VAR:-default}` form)
// =====================================================================

/// Replace `${NAME}` / `${NAME:-default}` references using the provided
/// resolver (a closure so tests can inject a fake env). This mirrors the
/// `${OTEL_ENDPOINT:-}` syntax shown in the design doc's YAML examples.
fn interpolate_env_vars<F>(input: &str, mut resolve: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            // find matching '}'
            if let Some(end) = input[i + 2..].find('}') {
                let inner = &input[i + 2..i + 2 + end];
                let (name, default) = match inner.find(":-") {
                    Some(idx) => (&inner[..idx], Some(&inner[idx + 2..])),
                    None => (inner, None),
                };
                let value = resolve(name)
                    .or_else(|| default.map(str::to_string))
                    .unwrap_or_default();
                out.push_str(&value);
                i += 2 + end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

// =====================================================================
//  4. init_tracing — the heart of the design.
// =====================================================================

/// Ensemble returned to `main` so it can flush + assert at the end.
struct TracingHandle {
    tracer_provider: Option<sdktrace::TracerProvider>,
    component_sink: Arc<ComponentLogSink>,
    otlp_sink: Option<OtlpSpanSink>,
}

fn init_tracing(cfg: &DrasiServerConfig, backend: OtlpBackend) -> Result<TracingHandle> {
    // ---- Layer 1: drasi-lib ComponentLogLayer ----------------------
    let (component_sink, component_layer) = init_component_log_layer();

    // ---- Layer 2: fmt -> stdout, filtered by logLevel --------------
    // Use the configured log_level as the EnvFilter directive (matches the
    // design's "preserve logLevel" requirement).
    let env_filter =
        EnvFilter::try_new(&cfg.log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_filter(env_filter);

    // ---- Layer 3: optional OTLP ------------------------------------
    let mut tracer_provider: Option<sdktrace::TracerProvider> = None;
    let mut otlp_sink: Option<OtlpSpanSink> = None;
    let mut otel_layer: Option<Box<dyn Layer<Registry> + Send + Sync>> = None;

    if let Some(tracing_cfg) = cfg.telemetry.as_ref().and_then(|t| t.tracing.as_ref()) {
        if !tracing_cfg.endpoint.is_empty() {
            let resource = Resource::new(vec![KeyValue::new(
                "service.name",
                tracing_cfg.service_name.clone(),
            )]);

            let provider = match backend {
                OtlpBackend::InMemorySimple => {
                    let sink = OtlpSpanSink::default();
                    let exporter = InMemoryOtlpExporter { sink: sink.clone() };
                    otlp_sink = Some(sink);
                    sdktrace::TracerProvider::builder()
                        .with_simple_exporter(exporter)
                        .with_config(sdktrace::config().with_resource(resource))
                        .build()
                }
                OtlpBackend::BrokenSimple { error_count } => {
                    let exporter = BrokenOtlpExporter { error_count };
                    sdktrace::TracerProvider::builder()
                        .with_simple_exporter(exporter)
                        .with_config(sdktrace::config().with_resource(resource))
                        .build()
                }
                OtlpBackend::InMemoryBatch => {
                    let sink = OtlpSpanSink::default();
                    let exporter = InMemoryOtlpExporter { sink: sink.clone() };
                    otlp_sink = Some(sink);
                    // Batch processor: spans queue in-memory and only flush on
                    // schedule (10s default) or `force_flush`. This is what
                    // real OTLP setups use, and is the path we need to verify
                    // for graceful-shutdown behavior.
                    sdktrace::TracerProvider::builder()
                        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
                        .with_config(sdktrace::config().with_resource(resource))
                        .build()
                }
            };

            let tracer = provider.tracer("drasi-server");
            // The `tracing-opentelemetry` bridge layer.
            let layer = tracing_opentelemetry::layer().with_tracer(tracer);
            otel_layer = Some(layer.boxed());
            tracer_provider = Some(provider);

            info!(
                otlp.endpoint = %tracing_cfg.endpoint,
                otlp.service_name = %tracing_cfg.service_name,
                "OTLP tracing enabled (in-memory stand-in for POC)"
            );
        }
    }

    // ---- Compose into a Registry-based subscriber ------------------
    // The boxed `otel_layer` only implements `Layer<Registry>`, so it must be
    // added while the inner subscriber type is still `Registry` — i.e. first.
    Registry::default()
        .with(otel_layer)
        .with(component_layer)
        .with(fmt_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to install global subscriber: {e}"))?;

    Ok(TracingHandle {
        tracer_provider,
        component_sink,
        otlp_sink,
    })
}

// =====================================================================
//  5. init_metrics — Prometheus scrape endpoint OR (no-op when disabled).
// =====================================================================

struct MetricsHandle {
    prometheus_addr: Option<SocketAddr>,
    prometheus_path: Option<String>,
}

fn init_metrics(cfg: &DrasiServerConfig) -> Result<MetricsHandle> {
    let metrics_cfg = match cfg.telemetry.as_ref().and_then(|t| t.metrics.as_ref()) {
        Some(m) => m,
        None => {
            info!("metrics: no telemetry.metrics configured -> facade calls are no-ops");
            return Ok(MetricsHandle {
                prometheus_addr: None,
                prometheus_path: None,
            });
        }
    };

    match metrics_cfg {
        MetricsConfig::Prometheus(p) => {
            let addr: SocketAddr = format!("127.0.0.1:{}", p.port).parse()?;
            let builder = metrics_exporter_prometheus::PrometheusBuilder::new()
                .with_http_listener(addr);
            // `install` spawns the listener on the current Tokio runtime.
            builder
                .install()
                .context("failed to install Prometheus recorder")?;
            info!(prometheus.addr = %addr, prometheus.path = %p.path,
                  "metrics: Prometheus scrape endpoint started");
            Ok(MetricsHandle {
                prometheus_addr: Some(addr),
                prometheus_path: Some(p.path.clone()),
            })
        }
        MetricsConfig::Otlp(o) => {
            // Real impl would build an OTLP metrics pipeline here. The POC
            // skips it to stay hermetic; we only verify the *config plumbing*
            // routes here.
            info!(otlp.endpoint = %o.endpoint, otlp.export_interval_s = o.export_interval,
                  "metrics: OTLP push selected (POC: no real exporter wired)");
            Ok(MetricsHandle {
                prometheus_addr: None,
                prometheus_path: None,
            })
        }
    }
}

// =====================================================================
//  6. SHUTDOWN HOOK
// =====================================================================

fn shutdown_telemetry(handle: TracingHandle) {
    if let Some(provider) = handle.tracer_provider {
        // Real drasi-server shutdown handler (SIGTERM/SIGINT) calls this so
        // pending spans flush before the process exits.
        let _ = provider.force_flush();
        // `shutdown` is on `TracerProvider` itself in 0.20.
        drop(provider);
    }
}

// =====================================================================
//  7. THE EXAMPLE CONFIG (replaces a YAML file load)
// =====================================================================

const EXAMPLE_CONFIG_YAML: &str = r#"
log_level: info
telemetry:
  tracing:
    endpoint: "http://jaeger:4317"
    service_name: "drasi-server-poc"
  metrics:
    backend: prometheus
    port: 9099
    path: "/metrics"
"#;

const EXAMPLE_CONFIG_DEFAULT_YAML: &str = r#"
log_level: info
"#;

/// YAML using the documented `${VAR:-default}` interpolation form (design §3.1).
const EXAMPLE_CONFIG_INTERP_YAML: &str = r#"
log_level: "${LOG_LEVEL:-info}"
telemetry:
  tracing:
    endpoint: "${OTEL_ENDPOINT:-}"
    service_name: "${SERVICE_NAME:-drasi-server}"
  metrics:
    backend: prometheus
    port: ${METRICS_PORT:-9090}
    path: "/metrics"
"#;

/// YAML that points at an unreachable OTLP endpoint. Real `opentelemetry-otlp`
/// would attempt and fail to connect; here we use `BrokenOtlpExporter` to
/// simulate the same property (every export fails) without external deps.
const EXAMPLE_CONFIG_UNREACHABLE_YAML: &str = r#"
log_level: info
telemetry:
  tracing:
    endpoint: "http://does-not-exist.invalid:4317"
    service_name: "drasi-server-poc"
"#;

/// YAML for the shutdown-flush scenario. Same shape as `enabled` but the
/// scenario will install a *batch* span processor instead of a simple one,
/// which is what real OTLP setups use.
const EXAMPLE_CONFIG_BATCH_YAML: &str = r#"
log_level: info
telemetry:
  tracing:
    endpoint: "http://jaeger:4317"
    service_name: "drasi-server-poc"
"#;

// =====================================================================
//  8. MAIN — dispatches to per-scenario verification.
// =====================================================================

#[tokio::main]
async fn main() -> Result<()> {
    println!("=========================================================");
    println!(" drasi-server telemetry wiring POC");
    println!("=========================================================\n");

    // Scenarios:
    //   enabled         (default)   - happy-path: composed subscriber + Prometheus
    //   default                     - no telemetry section -> backward-compat
    //   env_interp                  - verifies `${VAR:-default}` resolution
    //   unreachable                 - OTLP endpoint can't be reached -> graceful
    //   shutdown_flush              - BatchSpanProcessor flush on shutdown
    let scenario = std::env::var("POC_SCENARIO").unwrap_or_else(|_| "enabled".to_string());
    println!("Scenario: {scenario}\n");

    match scenario.as_str() {
        "default" => run_default_scenario().await,
        "env_interp" => run_env_interp_scenario().await,
        "unreachable" => run_unreachable_scenario().await,
        "shutdown_flush" => run_shutdown_flush_scenario().await,
        _ => run_enabled_scenario().await,
    }
}

// ---------------------------------------------------------------------
// Scenario: enabled (happy path) + default (no telemetry)
// ---------------------------------------------------------------------

async fn run_enabled_scenario() -> Result<()> {
    let config: DrasiServerConfig =
        serde_yaml::from_str(EXAMPLE_CONFIG_YAML).context("parsing example config")?;
    println!("Config YAML:{EXAMPLE_CONFIG_YAML}");

    let tracing_handle = init_tracing(&config, OtlpBackend::InMemorySimple)?;
    let metrics_handle = init_metrics(&config)?;
    emit_pipeline_telemetry();

    println!("\n--- Verification ---");
    assert_component_log_captured(&tracing_handle);
    assert_otlp_received_both_spans(&tracing_handle);
    assert_prometheus_endpoint_serves_metrics(&metrics_handle).await?;

    shutdown_telemetry(tracing_handle);
    println!("\nShutdown: tracer provider flushed.");
    println!("\nAll assertions passed (enabled scenario).");
    Ok(())
}

async fn run_default_scenario() -> Result<()> {
    let config: DrasiServerConfig =
        serde_yaml::from_str(EXAMPLE_CONFIG_DEFAULT_YAML).context("parsing default config")?;
    println!("Config YAML:{EXAMPLE_CONFIG_DEFAULT_YAML}");

    let tracing_handle = init_tracing(&config, OtlpBackend::InMemorySimple)?;
    let metrics_handle = init_metrics(&config)?;
    emit_pipeline_telemetry();

    println!("\n--- Verification ---");
    assert_component_log_captured(&tracing_handle);
    assert!(
        tracing_handle.otlp_sink.is_none(),
        "OTLP sink must not be installed when telemetry section is absent"
    );
    assert!(
        metrics_handle.prometheus_addr.is_none(),
        "Prometheus listener must not be installed when telemetry.metrics absent"
    );
    println!("  OK — no OTLP layer, no Prometheus listener (backward compat).");

    shutdown_telemetry(tracing_handle);
    println!("\nAll assertions passed (default scenario).");
    Ok(())
}

// ---------------------------------------------------------------------
// Scenario: env_interp — verifies `${VAR:-default}` resolution
// ---------------------------------------------------------------------

async fn run_env_interp_scenario() -> Result<()> {
    println!("Raw YAML (pre-interpolation):{EXAMPLE_CONFIG_INTERP_YAML}");

    // Inject a fake env: only some vars set, others must fall back to defaults.
    let env: std::collections::HashMap<&str, &str> = [
        ("OTEL_ENDPOINT", "http://jaeger.local:4317"),
        ("METRICS_PORT", "9100"),
        // LOG_LEVEL and SERVICE_NAME deliberately unset -> defaults must apply
    ]
    .into_iter()
    .collect();

    let interpolated = interpolate_env_vars(EXAMPLE_CONFIG_INTERP_YAML, |name| {
        env.get(name).map(|v| (*v).to_string())
    });
    println!("Interpolated YAML:{interpolated}");

    let config: DrasiServerConfig =
        serde_yaml::from_str(&interpolated).context("parsing interpolated config")?;

    println!("\n--- Verification ---");
    assert_eq!(
        config.log_level, "info",
        "LOG_LEVEL unset -> default 'info' must apply"
    );
    let tracing_cfg = config
        .telemetry
        .as_ref()
        .and_then(|t| t.tracing.as_ref())
        .expect("tracing section");
    assert_eq!(tracing_cfg.endpoint, "http://jaeger.local:4317");
    assert_eq!(tracing_cfg.service_name, "drasi-server");
    match config
        .telemetry
        .as_ref()
        .and_then(|t| t.metrics.as_ref())
        .expect("metrics section")
    {
        MetricsConfig::Prometheus(p) => {
            assert_eq!(p.port, 9100);
            assert_eq!(p.path, "/metrics");
        }
        _ => panic!("expected Prometheus backend"),
    }
    println!("  OK — env vars resolved (set values used, unset fall back to defaults).");

    println!("\nAll assertions passed (env_interp scenario).");
    Ok(())
}

// ---------------------------------------------------------------------
// Scenario: unreachable — OTLP endpoint can't be reached
// ---------------------------------------------------------------------

async fn run_unreachable_scenario() -> Result<()> {
    let config: DrasiServerConfig = serde_yaml::from_str(EXAMPLE_CONFIG_UNREACHABLE_YAML)
        .context("parsing unreachable config")?;
    println!("Config YAML:{EXAMPLE_CONFIG_UNREACHABLE_YAML}");

    let error_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend = OtlpBackend::BrokenSimple {
        error_count: error_count.clone(),
    };

    // Per the design's verification table:
    //   "Configure non-existent OTLP endpoint; verify server starts gracefully,
    //    logs warning"
    // i.e. init_tracing must NOT fail.
    let tracing_handle = init_tracing(&config, backend).context("init_tracing must not fail")?;
    println!("init_tracing returned Ok (server would have started normally).");

    // Emit a few spans — every export attempt errors, but the process must keep
    // running. With SimpleSpanProcessor, errors are logged and swallowed.
    emit_pipeline_telemetry();

    let errors = error_count.load(std::sync::atomic::Ordering::SeqCst);
    println!("\n--- Verification ---");
    println!("BrokenOtlpExporter saw {errors} failed export attempt(s).");
    assert!(
        errors >= 1,
        "expected at least one failed export attempt against the unreachable endpoint"
    );
    // ComponentLogLayer + fmt should still have worked despite OTLP being broken.
    assert_component_log_captured(&tracing_handle);
    println!("  OK — server still emits logs to other layers when OTLP is unreachable.");

    // Shutdown must also be graceful (no panics, no hangs).
    shutdown_telemetry(tracing_handle);
    println!("  OK — shutdown_telemetry returned cleanly with broken exporter.");

    println!("\nAll assertions passed (unreachable scenario).");
    Ok(())
}

// ---------------------------------------------------------------------
// Scenario: shutdown_flush — BatchSpanProcessor + force flush on shutdown
// ---------------------------------------------------------------------

async fn run_shutdown_flush_scenario() -> Result<()> {
    let config: DrasiServerConfig =
        serde_yaml::from_str(EXAMPLE_CONFIG_BATCH_YAML).context("parsing batch config")?;
    println!("Config YAML:{EXAMPLE_CONFIG_BATCH_YAML}");

    let tracing_handle = init_tracing(&config, OtlpBackend::InMemoryBatch)?;
    let sink = tracing_handle
        .otlp_sink
        .clone()
        .expect("batch backend installs an OTLP sink");

    // Emit pipeline telemetry. With a BatchSpanProcessor the spans should be
    // queued, NOT yet exported (default scheduled delay is several seconds).
    emit_pipeline_telemetry();

    // Give the batch processor a brief moment in case it had already started a
    // tick — but well below the default scheduled-delay (5s).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let pre_flush = sink.spans.lock().unwrap().len();
    println!("\n--- Verification ---");
    println!("Spans visible before shutdown flush: {pre_flush}");
    // We don't strictly assert pre_flush == 0 (the batch worker *could* tick
    // very quickly on a busy CI box), but we DO require the post-flush count
    // to be at least the number of spans emitted.

    // Shut down — this MUST flush remaining spans before returning.
    shutdown_telemetry(tracing_handle);

    let post_flush = sink.spans.lock().unwrap().len();
    println!("Spans visible after shutdown flush:  {post_flush}");
    assert!(
        post_flush >= 2,
        "expected shutdown to flush at least the 2 emitted spans (got {post_flush})"
    );
    println!("  OK — shutdown_telemetry flushed all pending spans.");

    println!("\nAll assertions passed (shutdown_flush scenario).");
    Ok(())
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

fn emit_pipeline_telemetry() {
    let span = info_span!("source.dispatch", source_id = "vehicle-source");
    let _e = span.enter();
    info!(
        target: "drasi_lib::sources",
        source_id = "vehicle-source",
        "dispatching change to query container"
    );
    metrics::counter!("drasi.source.events_dispatched", "source_id" => "vehicle-source")
        .increment(1);

    let inner = info_span!("query.process", query_id = "available-drivers");
    let _e2 = inner.enter();
    info!(
        target: "drasi_lib::queries",
        query_id = "available-drivers",
        "processed change"
    );
    metrics::histogram!("drasi.query.engine_duration_ns", "query_id" => "available-drivers")
        .record(42_000.0);
}

fn assert_component_log_captured(handle: &TracingHandle) {
    let component_events = handle.component_sink.events.lock().unwrap().clone();
    println!("ComponentLogLayer captured {} event(s).", component_events.len());
    assert!(
        component_events
            .iter()
            .any(|e| e.target.contains("drasi_lib::sources")
                && e.message.contains("dispatching change")),
        "ComponentLogLayer should have captured the source dispatch event"
    );
    assert!(
        component_events
            .iter()
            .any(|e| e.target.contains("drasi_lib::queries") && e.message.contains("processed")),
        "ComponentLogLayer should have captured the query process event"
    );
    println!("  OK — ComponentLogLayer received events from both pipeline stages.");
}

fn assert_otlp_received_both_spans(handle: &TracingHandle) {
    let sink = handle
        .otlp_sink
        .as_ref()
        .expect("OTLP sink must be installed");
    if let Some(provider) = handle.tracer_provider.as_ref() {
        let _ = provider.force_flush();
    }
    let spans = sink.spans.lock().unwrap().clone();
    println!("\nOTLP exporter received {} span(s).", spans.len());
    assert!(
        spans.iter().any(|s| s.name == "source.dispatch"),
        "OTLP should have received source.dispatch"
    );
    assert!(
        spans.iter().any(|s| s.name == "query.process"),
        "OTLP should have received query.process"
    );
    println!("  OK — OTLP exporter received both spans.");
}

async fn assert_prometheus_endpoint_serves_metrics(handle: &MetricsHandle) -> Result<()> {
    let (Some(addr), Some(path)) = (handle.prometheus_addr, handle.prometheus_path.as_ref()) else {
        panic!("Prometheus endpoint not installed");
    };
    let url = format!("http://{addr}{path}");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let body = reqwest::get(&url).await?.text().await?;
    println!(
        "\nPrometheus scrape ({url}) returned {} bytes; relevant lines:",
        body.len()
    );
    for line in body.lines().filter(|l| l.contains("drasi_")) {
        println!("  {line}");
    }
    assert!(
        body.contains("drasi_source_events_dispatched"),
        "Prometheus output should contain drasi_source_events_dispatched"
    );
    assert!(
        body.contains("drasi_query_engine_duration_ns"),
        "Prometheus output should contain drasi_query_engine_duration_ns"
    );
    println!("  OK — drasi-lib metrics visible via /metrics scrape.");
    Ok(())
}
