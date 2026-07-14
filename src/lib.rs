//! Standalone Octri monitoring for Rust applications.
//!
//! Reporting is fire-and-forget and best-effort. Transport failures are always
//! ignored so telemetry cannot affect the host application.

use chrono::Utc;
use rand::{rngs::OsRng, RngCore};
use serde_json::{json, Map, Value};
use std::backtrace::Backtrace;
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub url: String,
    /// Optional only for open self-hosted ingestion. Hosted Octri requires it.
    pub token: Option<String>,
    pub environment: String,
    pub release: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: String,
    pub parent_span_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct EventOptions {
    pub timestamp: Option<String>,
    pub level: Option<String>,
    pub operation_id: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status_code: Option<i64>,
    pub latency_ms: Option<f64>,
    pub attempt: Option<i64>,
    pub request_id: Option<String>,
    pub user: Option<BTreeMap<String, Value>>,
    pub tags: Option<BTreeMap<String, Value>>,
    pub context: Option<BTreeMap<String, Value>>,
    pub breadcrumbs: Option<Vec<Value>>,
    pub fingerprint: Option<String>,
    pub trace: Option<TraceContext>,
    pub span_id: Option<String>,
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ErrorOptions {
    pub level: Option<String>,
    pub operation_id: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status_code: Option<i64>,
    pub trace: Option<TraceContext>,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub service: String,
    pub operation_id: Option<String>,
    pub start_time: String,
    pub end_time: Option<String>,
    pub status: String,
}

static CONFIG: OnceLock<RwLock<Option<Config>>> = OnceLock::new();

fn config_cell() -> &'static RwLock<Option<Config>> {
    CONFIG.get_or_init(|| RwLock::new(None))
}

/// Configure Octri once during application startup.
pub fn init(mut config: Config) {
    config.url = config.url.trim_end_matches('/').to_string();
    if let Ok(mut current) = config_cell().write() {
        *current = Some(config);
    }
}

/// Read a W3C traceparent header or start a fresh trace.
pub fn trace_from_header(traceparent: Option<&str>) -> TraceContext {
    if let Some(value) = traceparent {
        let parts: Vec<_> = value.trim().split('-').collect();
        if parts.len() == 4
            && parts[0] == "00"
            && is_hex(parts[1], 32)
            && is_hex(parts[2], 16)
            && is_hex(parts[3], 2)
            && !all_zeros(parts[1])
            && !all_zeros(parts[2])
        {
            return TraceContext {
                trace_id: parts[1].to_ascii_lowercase(),
                parent_span_id: Some(parts[2].to_ascii_lowercase()),
            };
        }
    }
    TraceContext {
        trace_id: random_hex(16),
        parent_span_id: None,
    }
}

/// Log an application event without depending on a generated Octri API SDK.
pub fn capture_event(message: impl Into<String>, options: EventOptions) {
    let Some(config) = current_config() else {
        return;
    };
    let event_id = resolve_event_id(options.event_id);
    let mut payload = Map::new();
    payload.insert("eventId".into(), json!(event_id.clone()));
    payload.insert(
        "timestamp".into(),
        json!(options.timestamp.unwrap_or_else(now)),
    );
    payload.insert(
        "level".into(),
        json!(options.level.unwrap_or_else(|| "info".into())),
    );
    payload.insert("message".into(), json!(message.into()));
    payload.insert("environment".into(), json!(config.environment));
    insert(&mut payload, "release", config.release.clone());
    insert(&mut payload, "operationId", options.operation_id);
    insert(&mut payload, "method", options.method);
    insert(&mut payload, "path", options.path);
    insert(&mut payload, "statusCode", options.status_code);
    insert(&mut payload, "latencyMs", options.latency_ms);
    insert(&mut payload, "attempt", options.attempt);
    insert(&mut payload, "requestId", options.request_id);
    insert(&mut payload, "user", options.user);
    let mut tags = BTreeMap::from([("octri.origin".into(), json!("standalone"))]);
    if let Some(custom_tags) = options.tags {
        tags.extend(custom_tags);
    }
    payload.insert("tags".into(), json!(tags));
    insert(&mut payload, "context", options.context);
    insert(&mut payload, "breadcrumbs", options.breadcrumbs);
    insert(&mut payload, "fingerprint", options.fingerprint);
    if let Some(trace) = options.trace {
        payload.insert("traceId".into(), json!(trace.trace_id));
    }
    insert(&mut payload, "spanId", options.span_id);
    post(config, "/ingest", Value::Object(payload), event_id);
}

/// Capture a Rust error with a symbolic backtrace.
pub fn capture_error<E: Error + 'static>(error: &E, options: ErrorOptions) {
    let Some(config) = current_config() else {
        return;
    };
    let trace = options.trace.unwrap_or_else(|| trace_from_header(None));
    let event_id = random_hex(16);
    let mut payload = Map::new();
    payload.insert("eventId".into(), json!(event_id.clone()));
    payload.insert("timestamp".into(), json!(now()));
    payload.insert(
        "level".into(),
        json!(options.level.unwrap_or_else(|| "error".into())),
    );
    payload.insert("environment".into(), json!(config.environment));
    insert(&mut payload, "release", config.release.clone());
    payload.insert("traceId".into(), json!(trace.trace_id));
    payload.insert("spanId".into(), json!(random_hex(8)));
    payload.insert("tags".into(), json!({ "octri.origin": "server" }));
    payload.insert(
        "error".into(),
        json!({
            "name": error_type_name(error),
            "message": error.to_string(),
            "stack": Backtrace::force_capture().to_string(),
            "frames": [],
        }),
    );
    insert(&mut payload, "operationId", options.operation_id);
    insert(&mut payload, "method", options.method);
    insert(&mut payload, "path", options.path);
    insert(&mut payload, "statusCode", options.status_code);
    post(config, "/ingest", Value::Object(payload), event_id);
}

/// Report a completed distributed-trace span.
pub fn capture_span(span: Span) {
    let Some(config) = current_config() else {
        return;
    };
    if span.trace_id.is_empty()
        || span.span_id.is_empty()
        || span.name.is_empty()
        || span.start_time.is_empty()
    {
        return;
    }
    let key = format!("{}:{}", span.trace_id, span.span_id);
    let payload = json!({
        "traceId": span.trace_id,
        "spanId": span.span_id,
        "parentSpanId": span.parent_span_id,
        "environment": config.environment,
        "name": span.name,
        "service": if span.service.is_empty() { "server" } else { &span.service },
        "operationId": span.operation_id,
        "startTime": span.start_time,
        "endTime": span.end_time,
        "status": if span.status.is_empty() { "ok" } else { &span.status },
    });
    post(config, "/traces", payload, key);
}

fn current_config() -> Option<Config> {
    config_cell().read().ok().and_then(|value| value.clone())
}

fn post(config: Config, path: &'static str, payload: Value, idempotency_key: String) {
    if !safe_header_value(&idempotency_key)
        || config
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty() && !safe_header_value(token))
    {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("octri-monitoring".into())
        .spawn(move || {
            let mut request = ureq::post(&(config.url + path))
                .timeout(Duration::from_secs(5))
                .set("content-type", "application/json")
                .set("idempotency-key", &idempotency_key);
            if let Some(token) = config.token.filter(|value| !value.is_empty()) {
                request = request.set("authorization", &format!("Bearer {token}"));
            }
            let _ = request.send_json(payload);
        });
}

fn insert<T: serde::Serialize>(target: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        if let Ok(value) = serde_json::to_value(value) {
            target.insert(key.to_string(), value);
        }
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn all_zeros(value: &str) -> bool {
    value.bytes().all(|byte| byte == b'0')
}

fn safe_header_value(value: &str) -> bool {
    !value.is_empty() && !value.contains('\r') && !value.contains('\n')
}

fn resolve_event_id(value: Option<String>) -> String {
    value
        .filter(|candidate| safe_header_value(candidate))
        .unwrap_or_else(|| random_hex(16))
}

fn error_type_name<E: Error + 'static>(_: &E) -> &'static str {
    std::any::type_name::<E>()
}

fn random_hex(bytes: usize) -> String {
    let mut data = vec![0u8; bytes];
    OsRng.fill_bytes(&mut data);
    data.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_traceparent() {
        let trace = trace_from_header(Some(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        ));
        assert_eq!(trace.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(trace.parent_span_id.as_deref(), Some("00f067aa0ba902b7"));
    }

    #[test]
    fn rejects_zero_traceparent_identifiers() {
        let trace = trace_from_header(Some(
            "00-00000000000000000000000000000000-0000000000000000-01",
        ));
        assert_eq!(trace.trace_id.len(), 32);
        assert!(trace.trace_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(trace.trace_id, "0".repeat(32));
        assert_eq!(trace.parent_span_id, None);
    }

    #[test]
    fn replaces_unsafe_event_ids_and_keeps_concrete_error_type() {
        let event_id = resolve_event_id(Some("event\r\nX-Injected: true".into()));
        assert_eq!(event_id.len(), 32);
        assert!(safe_header_value(&event_id));

        let error = std::io::Error::new(std::io::ErrorKind::Other, "boom");
        assert!(error_type_name(&error).contains("std::io::error::Error"));
    }
}
