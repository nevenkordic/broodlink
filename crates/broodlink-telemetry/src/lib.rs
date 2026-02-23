/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 *
 * This program is free software: you can redistribute it
 * and/or modify it under the terms of the GNU Affero
 * General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your
 * option) any later version.
 *
 * This program is distributed in the hope that it will be
 * useful, but WITHOUT ANY WARRANTY; without even the
 * implied warranty of MERCHANTABILITY or FITNESS FOR A
 * PARTICULAR PURPOSE. See the GNU Affero General Public
 * License for more details.
 *
 * You should have received a copy of the GNU Affero General
 * Public License along with this program. If not, see
 * <https://www.gnu.org/licenses/>.
 */

#![allow(clippy::module_name_repetitions)]

use broodlink_config::TelemetryConfig;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::runtime::Tokio;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Errors from telemetry initialization.
#[derive(thiserror::Error, Debug)]
pub enum TelemetryError {
    #[error("opentelemetry setup failed: {0}")]
    Setup(String),
}

/// Guard that shuts down the `OTel` trace pipeline on drop.
/// Must be held for the lifetime of the application.
pub struct TelemetryGuard {
    provider: Option<opentelemetry_sdk::trace::TracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(ref provider) = self.provider {
            if let Err(e) = provider.shutdown() {
                eprintln!("telemetry shutdown error: {e}");
            }
        }
    }
}

/// Initialize the tracing subscriber with optional OpenTelemetry export.
///
/// When `config.enabled` is false (the default), sets up fmt-only output
/// identical to the existing behavior. When true, adds an OTLP export layer
/// and installs the W3C `TraceContext` propagator for cross-service correlation.
///
/// # Errors
///
/// Returns [`TelemetryError`] if the OTLP exporter cannot be created
/// (only when `enabled=true`).
pub fn init_telemetry(
    service_name: &str,
    config: &TelemetryConfig,
) -> Result<TelemetryGuard, TelemetryError> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_target(true)
        .with_thread_ids(true);

    if config.enabled {
        // Install W3C TraceContext propagator for cross-service trace correlation
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(&config.otlp_endpoint)
            .build()
            .map_err(|e| TelemetryError::Setup(format!("{e:?}")))?;

        let sampler = if (config.sample_rate - 1.0).abs() < f64::EPSILON {
            opentelemetry_sdk::trace::Sampler::AlwaysOn
        } else if config.sample_rate <= 0.0 {
            opentelemetry_sdk::trace::Sampler::AlwaysOff
        } else {
            opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(config.sample_rate)
        };

        let resource = opentelemetry_sdk::Resource::new(vec![KeyValue::new(
            "service.name",
            service_name.to_string(),
        )]);

        let provider = opentelemetry_sdk::trace::TracerProvider::builder()
            .with_batch_exporter(exporter, Tokio)
            .with_sampler(sampler)
            .with_resource(resource)
            .build();

        let tracer = provider.tracer(service_name.to_string());
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(otel_layer)
            .init();

        Ok(TelemetryGuard {
            provider: Some(provider),
        })
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .init();

        Ok(TelemetryGuard { provider: None })
    }
}

// ---------------------------------------------------------------------------
// W3C TraceContext propagation helpers
// ---------------------------------------------------------------------------

/// Adapter to extract W3C trace context from HTTP headers.
struct HeaderExtractor<'a>(&'a http::HeaderMap);

impl opentelemetry::propagation::Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(http::HeaderName::as_str).collect()
    }
}

/// Adapter to inject W3C trace context into HTTP headers.
struct HeaderInjector<'a>(&'a mut http::HeaderMap);

impl opentelemetry::propagation::Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let Ok(name) = http::header::HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(val) = http::header::HeaderValue::from_str(&value) {
                self.0.insert(name, val);
            }
        }
    }
}

/// Extract W3C `TraceContext` from incoming HTTP headers and return an `OTel` context.
/// Use this in middleware to link incoming requests to the distributed trace.
#[must_use]
pub fn extract_trace_context(headers: &http::HeaderMap) -> opentelemetry::Context {
    let extractor = HeaderExtractor(headers);
    opentelemetry::global::get_text_map_propagator(|propagator| propagator.extract(&extractor))
}

/// Inject W3C `TraceContext` headers (`traceparent`, `tracestate`) into outbound HTTP headers.
/// Use this before making outbound HTTP calls to propagate the trace.
pub fn inject_trace_context(cx: &opentelemetry::Context, headers: &mut http::HeaderMap) {
    let mut injector = HeaderInjector(headers);
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(cx, &mut injector);
    });
}

/// Get the current OTLP trace ID from the active tracing span.
/// Returns the 32-hex-char trace ID if telemetry is active, otherwise returns None.
#[must_use]
pub fn current_trace_id() -> Option<String> {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let span = tracing::Span::current();
    let cx = span.context();
    let span_ref = cx.span();
    let trace_id = span_ref.span_context().trace_id();

    if trace_id == opentelemetry::trace::TraceId::INVALID {
        None
    } else {
        Some(trace_id.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_config_disabled_by_default() {
        let config = TelemetryConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.otlp_endpoint, "http://localhost:4317");
        assert!((config.sample_rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_guard_drop_withoutprovider() {
        let guard = TelemetryGuard { provider: None };
        drop(guard); // Must not panic
    }

    #[test]
    fn test_telemetry_error_display() {
        let err = TelemetryError::Setup("test failure".to_string());
        assert_eq!(err.to_string(), "opentelemetry setup failed: test failure");
    }

    #[test]
    fn test_config_default_values_match_spec() {
        let config = TelemetryConfig::default();
        assert!(!config.enabled, "telemetry disabled by default");
        assert_eq!(
            config.otlp_endpoint, "http://localhost:4317",
            "OTLP gRPC default port"
        );
        assert!(
            (config.sample_rate - 1.0).abs() < f64::EPSILON,
            "sample all by default"
        );
    }

    #[test]
    fn test_config_deserialize_from_toml() {
        let toml_str = r#"
enabled = true
otlp_endpoint = "http://jaeger:4317"
sample_rate = 0.5
"#;
        let config: TelemetryConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.otlp_endpoint, "http://jaeger:4317");
        assert!((config.sample_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_current_trace_id_returns_none_without_otel() {
        // Without telemetry initialized, should return None
        assert!(current_trace_id().is_none());
    }

    #[test]
    fn test_header_injector_roundtrip() {
        use opentelemetry::propagation::Injector;
        let mut headers = http::HeaderMap::new();
        let mut injector = HeaderInjector(&mut headers);
        injector.set("traceparent", "00-abc123-def456-01".to_string());
        assert_eq!(
            headers.get("traceparent").and_then(|v| v.to_str().ok()),
            Some("00-abc123-def456-01")
        );
    }
}
