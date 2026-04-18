mod otlp;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gethostname::gethostname;
use kiliax_core::config::{OtelCaptureMode, OtelConfig, OtelOtlpProtocol};
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator as _;
use opentelemetry::trace::TraceContextExt as _;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::tonic_types::metadata::MetadataMap;
use opentelemetry_otlp::tonic_types::transport::ClientTlsConfig;
use opentelemetry_otlp::LogExporter;
use opentelemetry_otlp::Protocol;
use opentelemetry_otlp::SpanExporter;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_otlp::WithTonicConfig;
use opentelemetry_otlp::OTEL_EXPORTER_OTLP_LOGS_TIMEOUT;
use opentelemetry_otlp::OTEL_EXPORTER_OTLP_METRICS_TIMEOUT;
use opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_TIMEOUT;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::PeriodicReader;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor as TokioBatchSpanProcessor;
use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions as semconv;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::Layer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalLogs {
    None,
    Stdout,
    File { path: PathBuf },
}

#[derive(Default)]
pub struct OtelGuard {
    provider: Option<OtelProvider>,
}

impl OtelGuard {
    pub fn shutdown(self) {}
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            provider.shutdown();
        }
    }
}

#[derive(Clone)]
struct OtelProvider {
    logger: Option<SdkLoggerProvider>,
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl OtelProvider {
    fn shutdown(&self) {
        if let Some(tracer_provider) = &self.tracer_provider {
            let _ = tracer_provider.force_flush();
            let _ = tracer_provider.shutdown();
        }
        if let Some(meter_provider) = &self.meter_provider {
            let _ = meter_provider.shutdown();
        }
        if let Some(logger) = &self.logger {
            let _ = logger.shutdown();
        }
    }

    fn logger_layer<S>(&self) -> Option<impl Layer<S> + Send + Sync>
    where
        S: tracing::Subscriber + for<'span> LookupSpan<'span> + Send + Sync,
    {
        self.logger.as_ref().map(|logger| {
            OpenTelemetryTracingBridge::new(logger).with_filter(filter_fn(OtelProvider::log_filter))
        })
    }

    fn tracing_layer<S>(&self, service_name: &str) -> Option<impl Layer<S> + Send + Sync>
    where
        S: tracing::Subscriber + for<'span> LookupSpan<'span> + Send + Sync,
    {
        self.tracer_provider.as_ref().map(|provider| {
            let tracer = provider.tracer(service_name.to_string());
            tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(filter_fn(OtelProvider::trace_filter))
        })
    }

    fn log_filter(meta: &tracing::Metadata<'_>) -> bool {
        is_kiliax_target(meta.target())
    }

    fn trace_filter(meta: &tracing::Metadata<'_>) -> bool {
        meta.is_span() && is_kiliax_target(meta.target())
    }
}

#[derive(Clone)]
struct LockedFileWriter(Arc<Mutex<std::fs::File>>);

impl std::io::Write for LockedFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("kiliax-otel log file lock poisoned")
            .write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0
            .lock()
            .expect("kiliax-otel log file lock poisoned")
            .flush()
    }
}

#[derive(Clone)]
struct MakeLockedFileWriter(LockedFileWriter);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeLockedFileWriter {
    type Writer = LockedFileWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.0.clone()
    }
}

fn file_writer(path: &Path) -> anyhow::Result<MakeLockedFileWriter> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    Ok(MakeLockedFileWriter(LockedFileWriter(Arc::new(
        Mutex::new(file),
    ))))
}

pub fn init(
    cfg: &kiliax_core::config::Config,
    service_name: &'static str,
    service_version: &'static str,
    local_logs: LocalLogs,
) -> anyhow::Result<OtelGuard> {
    // Update capture behavior in `kiliax-core` early (even if exporter disabled).
    kiliax_core::telemetry::set_capture_config(
        cfg.otel.enabled.then_some(cfg.otel.capture.clone()),
    );

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    if !cfg.otel.enabled {
        match local_logs {
            LocalLogs::None => {}
            LocalLogs::Stdout => {
                let fmt_layer = tracing_subscriber::fmt::layer()
                    .json()
                    .with_timer(tracing_subscriber::fmt::time::SystemTime)
                    .with_ansi(false)
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(true);
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt_layer)
                    .try_init()
                    .ok();
            }
            LocalLogs::File { path } => {
                if let Ok(writer) = file_writer(&path) {
                    let fmt_layer = tracing_subscriber::fmt::layer()
                        .json()
                        .with_timer(tracing_subscriber::fmt::time::SystemTime)
                        .with_ansi(false)
                        .flatten_event(true)
                        .with_current_span(true)
                        .with_span_list(true)
                        .with_writer(writer);
                    tracing_subscriber::registry()
                        .with(env_filter)
                        .with(fmt_layer)
                        .try_init()
                        .ok();
                }
            }
        }
        return Ok(OtelGuard::default());
    }

    let provider = build_provider(&cfg.otel, service_name, service_version)?;

    if let Some(tp) = provider.tracer_provider.clone() {
        global::set_tracer_provider(tp);
        global::set_text_map_propagator(TraceContextPropagator::new());
    }
    if let Some(mp) = provider.meter_provider.clone() {
        global::set_meter_provider(mp);
    }

    match local_logs {
        LocalLogs::None => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(provider.tracing_layer(service_name))
                .with(provider.logger_layer())
                .try_init()
                .ok();
        }
        LocalLogs::Stdout => {
            let fmt_layer = tracing_subscriber::fmt::layer()
                .json()
                .with_timer(tracing_subscriber::fmt::time::SystemTime)
                .with_ansi(false)
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(true);
            tracing_subscriber::registry()
                .with(env_filter)
                .with(provider.tracing_layer(service_name))
                .with(provider.logger_layer())
                .with(fmt_layer)
                .try_init()
                .ok();
        }
        LocalLogs::File { path } => match file_writer(&path) {
            Ok(writer) => {
                let fmt_layer = tracing_subscriber::fmt::layer()
                    .json()
                    .with_timer(tracing_subscriber::fmt::time::SystemTime)
                    .with_ansi(false)
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_writer(writer);
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(provider.tracing_layer(service_name))
                    .with(provider.logger_layer())
                    .with(fmt_layer)
                    .try_init()
                    .ok();
            }
            Err(err) => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(provider.tracing_layer(service_name))
                    .with(provider.logger_layer())
                    .try_init()
                    .ok();
                tracing::warn!(
                    target: "kiliax_otel",
                    event = "local_log_file_failed",
                    path = %path.display(),
                    error = %err,
                );
            }
        },
    }

    if matches!(cfg.otel.capture.mode, OtelCaptureMode::Full) {
        tracing::info!(
            target: "kiliax_otel",
            event = "otel_enabled",
            capture = "full",
            endpoint = %cfg.otel.otlp.endpoint,
            protocol = ?cfg.otel.otlp.protocol,
        );
    }

    Ok(OtelGuard {
        provider: Some(provider),
    })
}

pub fn set_parent_from_http_headers(span: &tracing::Span, headers: &http::HeaderMap) -> bool {
    let traceparent = headers.get("traceparent").and_then(|v| v.to_str().ok());
    let tracestate = headers.get("tracestate").and_then(|v| v.to_str().ok());
    set_parent_from_trace_headers(span, traceparent, tracestate)
}

pub fn set_parent_from_trace_headers(
    span: &tracing::Span,
    traceparent: Option<&str>,
    tracestate: Option<&str>,
) -> bool {
    let Some(traceparent) = traceparent else {
        return false;
    };

    let mut headers = std::collections::HashMap::new();
    headers.insert("traceparent".to_string(), traceparent.to_string());
    if let Some(tracestate) = tracestate {
        headers.insert("tracestate".to_string(), tracestate.to_string());
    }

    let context = TraceContextPropagator::new().extract(&headers);
    if !context.span().span_context().is_valid() {
        return false;
    }

    let _ = span.set_parent(context);
    true
}

fn build_provider(
    cfg: &OtelConfig,
    service_name: &str,
    service_version: &str,
) -> anyhow::Result<OtelProvider> {
    let resource = make_resource(cfg, service_name, service_version);

    let logger = cfg
        .signals
        .logs
        .then(|| build_logger_provider(cfg, &resource))
        .transpose()?;

    let tracer_provider = cfg
        .signals
        .traces
        .then(|| build_tracer_provider(cfg, &resource, service_name))
        .transpose()?;

    let meter_provider = cfg
        .signals
        .metrics
        .then(|| build_meter_provider(cfg, &resource))
        .transpose()?;

    Ok(OtelProvider {
        logger,
        tracer_provider,
        meter_provider,
    })
}

fn make_resource(cfg: &OtelConfig, service_name: &str, service_version: &str) -> Resource {
    let mut attributes = vec![
        KeyValue::new(
            semconv::attribute::SERVICE_VERSION,
            service_version.to_string(),
        ),
        KeyValue::new("deployment.environment.name", cfg.environment.clone()),
    ];

    let host = gethostname();
    let host = host.to_string_lossy();
    let host = host.trim();
    if !host.is_empty() {
        attributes.push(KeyValue::new("host.name", host.to_string()));
    }

    Resource::builder()
        .with_service_name(service_name.to_string())
        .with_attributes(attributes)
        .build()
}

fn protocol_for_http(cfg: &OtelConfig) -> Protocol {
    match cfg.otlp.protocol {
        OtelOtlpProtocol::HttpProtobuf => Protocol::HttpBinary,
        OtelOtlpProtocol::HttpJson => Protocol::HttpJson,
        OtelOtlpProtocol::Grpc => Protocol::Grpc,
    }
}

fn endpoint_for_signal(base: &str, signal: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/v1/{signal}")
}

fn build_logger_provider(
    cfg: &OtelConfig,
    resource: &Resource,
) -> anyhow::Result<SdkLoggerProvider> {
    let mut builder = SdkLoggerProvider::builder().with_resource(resource.clone());

    match cfg.otlp.protocol {
        OtelOtlpProtocol::Grpc => {
            let endpoint = cfg.otlp.endpoint.clone();
            let header_map = otlp::build_header_map(&cfg.otlp.headers);
            let metadata = MetadataMap::from_headers(header_map);

            let mut exporter = LogExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .with_metadata(metadata);

            if endpoint.starts_with("https://") || cfg.otlp.tls.is_some() {
                let base_tls_config = ClientTlsConfig::new()
                    .with_enabled_roots()
                    .assume_http2(true);
                let tls_config = match cfg.otlp.tls.as_ref() {
                    Some(tls) => otlp::build_grpc_tls_config(&endpoint, base_tls_config, tls)?,
                    None => base_tls_config,
                };
                exporter = exporter.with_tls_config(tls_config);
            }

            builder = builder.with_batch_exporter(exporter.build()?);
        }
        OtelOtlpProtocol::HttpProtobuf | OtelOtlpProtocol::HttpJson => {
            let endpoint = endpoint_for_signal(&cfg.otlp.endpoint, "logs");
            let mut exporter = LogExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(protocol_for_http(cfg))
                .with_headers(otlp::headers_to_hashmap(&cfg.otlp.headers));

            if let Some(tls) = cfg.otlp.tls.as_ref() {
                let client = otlp::build_http_client(tls, OTEL_EXPORTER_OTLP_LOGS_TIMEOUT)?;
                exporter = exporter.with_http_client(client);
            }

            builder = builder.with_batch_exporter(exporter.build()?);
        }
    }

    Ok(builder.build())
}

fn build_tracer_provider(
    cfg: &OtelConfig,
    resource: &Resource,
    service_name: &str,
) -> anyhow::Result<SdkTracerProvider> {
    match cfg.otlp.protocol {
        OtelOtlpProtocol::Grpc => {
            let endpoint = cfg.otlp.endpoint.clone();
            let header_map = otlp::build_header_map(&cfg.otlp.headers);
            let metadata = MetadataMap::from_headers(header_map);

            let mut exporter = SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .with_metadata(metadata);

            if endpoint.starts_with("https://") || cfg.otlp.tls.is_some() {
                let base_tls_config = ClientTlsConfig::new()
                    .with_enabled_roots()
                    .assume_http2(true);
                let tls_config = match cfg.otlp.tls.as_ref() {
                    Some(tls) => otlp::build_grpc_tls_config(&endpoint, base_tls_config, tls)?,
                    None => base_tls_config,
                };
                exporter = exporter.with_tls_config(tls_config);
            }

            let processor = BatchSpanProcessor::builder(exporter.build()?).build();
            Ok(SdkTracerProvider::builder()
                .with_resource(resource.clone())
                .with_span_processor(processor)
                .build())
        }
        OtelOtlpProtocol::HttpProtobuf | OtelOtlpProtocol::HttpJson => {
            let endpoint = endpoint_for_signal(&cfg.otlp.endpoint, "traces");

            if otlp::current_tokio_runtime_is_multi_thread() {
                let mut exporter = SpanExporter::builder()
                    .with_http()
                    .with_endpoint(endpoint)
                    .with_protocol(protocol_for_http(cfg))
                    .with_headers(otlp::headers_to_hashmap(&cfg.otlp.headers));

                let client = otlp::build_async_http_client(
                    cfg.otlp.tls.as_ref(),
                    OTEL_EXPORTER_OTLP_TRACES_TIMEOUT,
                )?;
                exporter = exporter.with_http_client(client);

                let tracer = SdkTracerProvider::builder()
                    .with_resource(resource.clone())
                    .with_span_processor(
                        TokioBatchSpanProcessor::builder(exporter.build()?, runtime::Tokio).build(),
                    )
                    .build();
                let _ = tracer.tracer(service_name.to_string());
                return Ok(tracer);
            }

            let mut exporter = SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(protocol_for_http(cfg))
                .with_headers(otlp::headers_to_hashmap(&cfg.otlp.headers));

            if let Some(tls) = cfg.otlp.tls.as_ref() {
                let client = otlp::build_http_client(tls, OTEL_EXPORTER_OTLP_TRACES_TIMEOUT)?;
                exporter = exporter.with_http_client(client);
            }

            let processor = BatchSpanProcessor::builder(exporter.build()?).build();
            Ok(SdkTracerProvider::builder()
                .with_resource(resource.clone())
                .with_span_processor(processor)
                .build())
        }
    }
}

fn build_meter_provider(cfg: &OtelConfig, resource: &Resource) -> anyhow::Result<SdkMeterProvider> {
    let reader = match cfg.otlp.protocol {
        OtelOtlpProtocol::Grpc => {
            let endpoint = cfg.otlp.endpoint.clone();
            let header_map = otlp::build_header_map(&cfg.otlp.headers);
            let metadata = MetadataMap::from_headers(header_map);

            let mut exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .with_metadata(metadata);

            if endpoint.starts_with("https://") || cfg.otlp.tls.is_some() {
                let base_tls_config = ClientTlsConfig::new()
                    .with_enabled_roots()
                    .assume_http2(true);
                let tls_config = match cfg.otlp.tls.as_ref() {
                    Some(tls) => otlp::build_grpc_tls_config(&endpoint, base_tls_config, tls)?,
                    None => base_tls_config,
                };
                exporter = exporter.with_tls_config(tls_config);
            }

            PeriodicReader::builder(exporter.build()?)
                .with_interval(Duration::from_secs(10))
                .build()
        }
        OtelOtlpProtocol::HttpProtobuf | OtelOtlpProtocol::HttpJson => {
            let endpoint = endpoint_for_signal(&cfg.otlp.endpoint, "metrics");
            let mut exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(protocol_for_http(cfg))
                .with_headers(otlp::headers_to_hashmap(&cfg.otlp.headers));

            if let Some(tls) = cfg.otlp.tls.as_ref() {
                let client = otlp::build_http_client(tls, OTEL_EXPORTER_OTLP_METRICS_TIMEOUT)?;
                exporter = exporter.with_http_client(client);
            }

            PeriodicReader::builder(exporter.build()?)
                .with_interval(Duration::from_secs(10))
                .build()
        }
    };

    Ok(SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_reader(reader)
        .build())
}

fn is_kiliax_target(target: &str) -> bool {
    target.starts_with("kiliax")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn unique_tmp_path(prefix: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}_{pid}_{n}"))
    }

    #[test]
    fn local_log_file_failure_is_non_fatal() {
        let parent_as_file = unique_tmp_path("kiliax_otel_test_parent_is_file");
        std::fs::write(&parent_as_file, b"not a dir").unwrap();
        let log_path = parent_as_file.join("tui.jsonl");

        let cfg = kiliax_core::config::Config::default();
        let res = init(
            &cfg,
            "kiliax-test",
            "0.0.0",
            LocalLogs::File { path: log_path },
        );

        let _ = std::fs::remove_file(&parent_as_file);
        assert!(res.is_ok());
    }
}
