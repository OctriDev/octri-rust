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

const MAX_IDEMPOTENCY_KEY_LENGTH: usize = 256;

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
    if !safe_idempotency_key(&idempotency_key)
        || config
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty() && !safe_header_value(token))
    {
        return;
    }
    let Some(payload) = scrub_payload(payload) else {
        return;
    };
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

// ── Scrubbing ────────────────────────────────────────────────────────────────

/// Keys whose value never leaves the process. Compared against the key with case
/// and separators removed, so `api_key`, `apiKey` and `API-KEY` all match
/// `apikey`, and the test is a substring one, so `stripe_secret_key` matches too.
const SCRUB_KEYS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "authorization",
    "credential",
    "cookie",
    "session",
    "privatekey",
    "accesskey",
    "cardnumber",
    "creditcard",
    "cvv",
    "ssn",
];

const REDACTED: &str = "[redacted]";
const TRUNCATED: &str = "[truncated]";
/// Deep enough for real context objects, shallow enough to stay cheap.
const MAX_SCRUB_DEPTH: usize = 8;

type BeforeSend = Box<dyn Fn(Value) -> Option<Value> + Send + Sync>;

static EXTRA_SCRUB_KEYS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
static BEFORE_SEND: OnceLock<RwLock<Option<BeforeSend>>> = OnceLock::new();

fn extra_scrub_keys() -> &'static RwLock<Vec<String>> {
    EXTRA_SCRUB_KEYS.get_or_init(|| RwLock::new(Vec::new()))
}

fn before_send_cell() -> &'static RwLock<Option<BeforeSend>> {
    BEFORE_SEND.get_or_init(|| RwLock::new(None))
}

/// Redacts more key names, on top of the built-in list. Matching ignores case
/// and separators and is a substring test, so `account` also covers
/// `account_number`.
///
/// ```no_run
/// octri_monitoring::add_scrub_fields(["account_number", "otp"]);
/// ```
pub fn add_scrub_fields<I, S>(fields: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if let Ok(mut keys) = extra_scrub_keys().write() {
        for field in fields {
            let key = normalize_key(field.as_ref());
            if !key.is_empty() && !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
}

/// Runs a hook on every payload just before it is sent. Return the payload to
/// send it, or `None` to drop the event.
///
/// ```no_run
/// octri_monitoring::set_before_send(|payload| {
///     if payload["path"] == "/health" {
///         return None;
///     }
///     Some(payload)
/// });
/// ```
///
/// Redaction still runs afterwards, so a hook cannot leak a credential by
/// accident. Use [`clear_before_send`] to remove it.
pub fn set_before_send<F>(hook: F)
where
    F: Fn(Value) -> Option<Value> + Send + Sync + 'static,
{
    if let Ok(mut slot) = before_send_cell().write() {
        *slot = Some(Box::new(hook));
    }
}

/// Removes the hook installed by [`set_before_send`].
pub fn clear_before_send() {
    if let Ok(mut slot) = before_send_cell().write() {
        *slot = None;
    }
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn is_secret_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    if normalized.is_empty() {
        return false;
    }
    if SCRUB_KEYS
        .iter()
        .any(|candidate| normalized.contains(candidate))
    {
        return true;
    }
    extra_scrub_keys()
        .read()
        .map(|keys| {
            keys.iter()
                .any(|candidate| normalized.contains(candidate.as_str()))
        })
        .unwrap_or(false)
}

/// Tells a card number from the order ids and timestamps that look like one.
fn passes_luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut double = false;
    for byte in digits.bytes().rev() {
        let mut digit = u32::from(byte - b'0');
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    sum % 10 == 0
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn at_word_start(bytes: &[u8], start: usize) -> bool {
    start == 0 || !is_word_byte(bytes[start - 1])
}

/// `Authorization: Bearer <token>` copied into a log line.
fn match_bearer(bytes: &[u8], start: usize) -> Option<usize> {
    if !at_word_start(bytes, start) || !bytes.get(start..start + 6)?.eq_ignore_ascii_case(b"bearer")
    {
        return None;
    }
    let mut index = start + 6;
    let gap = index;
    while matches!(bytes.get(index), Some(b' ' | b'\t')) {
        index += 1;
    }
    if index == gap {
        return None;
    }
    let token = index;
    while matches!(bytes.get(index), Some(byte) if byte.is_ascii_alphanumeric()
        || matches!(byte, b'_' | b'.' | b'~' | b'+' | b'/' | b'-' | b'='))
    {
        index += 1;
    }
    (index > token).then_some(index)
}

/// A three-segment JWT, which always starts `eyJ` once base64url-encoded.
fn match_jwt(bytes: &[u8], start: usize) -> Option<usize> {
    if !at_word_start(bytes, start) || bytes.get(start..start + 3)? != b"eyJ" {
        return None;
    }
    let mut index = start;
    for segment in 0..3 {
        if segment > 0 {
            if bytes.get(index) != Some(&b'.') {
                return None;
            }
            index += 1;
        }
        let from = index;
        while matches!(bytes.get(index), Some(byte) if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_'))
        {
            index += 1;
        }
        if index == from {
            return None;
        }
    }
    Some(index)
}

fn match_email(bytes: &[u8], start: usize) -> Option<usize> {
    if !at_word_start(bytes, start) {
        return None;
    }
    let mut index = start;
    while matches!(bytes.get(index), Some(byte) if byte.is_ascii_alphanumeric()
        || matches!(byte, b'.' | b'%' | b'+' | b'-' | b'_'))
    {
        index += 1;
    }
    if index == start || bytes.get(index) != Some(&b'@') {
        return None;
    }
    index += 1;
    let mut labels = 0;
    let mut end = index;
    loop {
        let from = index;
        while matches!(bytes.get(index), Some(byte) if byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            index += 1;
        }
        if index == from {
            break;
        }
        labels += 1;
        end = index;
        if bytes.get(index) == Some(&b'.') {
            index += 1;
        } else {
            break;
        }
    }
    // A bare host is not an address; `ada@example.com` needs at least two labels.
    (labels >= 2).then_some(end)
}

/// 13 to 19 digits, optionally grouped with spaces or hyphens, that also pass
/// the Luhn check.
fn match_card_number(bytes: &[u8], start: usize) -> Option<usize> {
    if !bytes.get(start)?.is_ascii_digit() || !at_word_start(bytes, start) {
        return None;
    }
    let mut index = start;
    let mut end = start;
    let mut digits = String::new();
    while let Some(&byte) = bytes.get(index) {
        if byte.is_ascii_digit() {
            digits.push(char::from(byte));
            index += 1;
            end = index;
            if digits.len() == 19 {
                break;
            }
        } else if matches!(byte, b' ' | b'-') {
            index += 1;
        } else {
            break;
        }
    }
    if digits.len() < 13 || bytes.get(end).is_some_and(|byte| is_word_byte(*byte)) {
        return None;
    }
    passes_luhn(&digits).then_some(end)
}

/// Replaces every span a matcher reports with `[redacted]`. Matchers only ever
/// start and end on ASCII bytes, so the slices stay on character boundaries.
fn replace_matches(value: &str, matcher: fn(&[u8], usize) -> Option<usize>) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    let mut copied = 0;
    while index < bytes.len() {
        match matcher(bytes, index) {
            Some(end) if end > index => {
                out.push_str(&value[copied..index]);
                out.push_str(REDACTED);
                index = end;
                copied = end;
            }
            _ => index += 1,
        }
    }
    out.push_str(&value[copied..]);
    out
}

/// Removes credentials and personal data that leaked into free text.
fn scrub_text(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let value = replace_matches(value, match_bearer);
    let value = replace_matches(&value, match_jwt);
    let value = replace_matches(&value, match_card_number);
    replace_matches(&value, match_email)
}

/// Redacts credential-shaped keys anywhere in the payload, and strips secrets
/// out of the free text around them. `user` is the field you deliberately fill
/// with an identity, so its strings are left alone; its keys are still checked.
fn scrub_value(value: Value, depth: usize, text: bool) -> Value {
    match value {
        Value::String(inner) => {
            if text {
                Value::String(scrub_text(&inner))
            } else {
                Value::String(inner)
            }
        }
        Value::Array(items) => {
            if depth >= MAX_SCRUB_DEPTH {
                return Value::String(TRUNCATED.to_string());
            }
            Value::Array(
                items
                    .into_iter()
                    .map(|item| scrub_value(item, depth + 1, text))
                    .collect(),
            )
        }
        Value::Object(fields) => {
            if depth >= MAX_SCRUB_DEPTH {
                return Value::String(TRUNCATED.to_string());
            }
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, nested)| {
                        if is_secret_key(&key) {
                            return (key, Value::String(REDACTED.to_string()));
                        }
                        let text = text && key != "user";
                        (key, scrub_value(nested, depth + 1, text))
                    })
                    .collect(),
            )
        }
        other => other,
    }
}

/// The last thing every payload passes through. Both the hook and the redaction
/// live here rather than in the capture functions, so nothing can be reported
/// around them.
fn scrub_payload(payload: Value) -> Option<Value> {
    let hooked = match before_send_cell().read() {
        Ok(slot) => match slot.as_ref() {
            Some(hook) => hook(payload)?,
            None => payload,
        },
        Err(_) => payload,
    };
    Some(scrub_value(hooked, 0, true))
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

/// A caller-supplied event id becomes the `idempotency-key` header, so it is
/// bounded as well as newline-free.
fn safe_idempotency_key(value: &str) -> bool {
    safe_header_value(value) && value.len() <= MAX_IDEMPOTENCY_KEY_LENGTH
}

fn resolve_event_id(value: Option<String>) -> String {
    value
        .filter(|candidate| safe_idempotency_key(candidate))
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

    /// The scrubber reads process-wide settings, so the tests that change them
    /// run one at a time instead of racing each other.
    static SCRUB_SETTINGS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scrub_settings() -> std::sync::MutexGuard<'static, ()> {
        SCRUB_SETTINGS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
    fn replaces_oversized_event_ids() {
        let event_id = resolve_event_id(Some("e".repeat(MAX_IDEMPOTENCY_KEY_LENGTH + 1)));
        assert_eq!(event_id.len(), 32);
        assert!(safe_idempotency_key(&event_id));
    }

    #[test]
    fn redacts_credential_shaped_keys_however_they_are_spelled() {
        let _guard = scrub_settings();
        let payload = scrub_payload(json!({
            "context": {
                "api_key": "sk_live_1",
                "apiKey": "sk_live_2",
                "X-API-KEY": "sk_live_3",
                "stripe_secret_key": "sk_live_4",
                "Authorization": "Bearer abc",
                "refresh_token": "rt_1",
                "cookie": "sid=1",
                "orderId": "A-1024",
                "author": "ada",
            }
        }))
        .expect("payload");

        let context = &payload["context"];
        for key in [
            "api_key",
            "apiKey",
            "X-API-KEY",
            "stripe_secret_key",
            "Authorization",
            "refresh_token",
            "cookie",
        ] {
            assert_eq!(context[key], json!(REDACTED), "{key}");
        }
        assert_eq!(context["orderId"], json!("A-1024"));
        assert_eq!(context["author"], json!("ada"));
    }

    #[test]
    fn redacts_nested_and_array_values() {
        let _guard = scrub_settings();
        let payload = scrub_payload(json!({
            "context": { "upstream": { "headers": [{ "authorization": "Bearer abc" }] } }
        }))
        .expect("payload");

        assert_eq!(
            payload["context"]["upstream"]["headers"][0]["authorization"],
            json!(REDACTED)
        );
    }

    #[test]
    fn add_scrub_fields_is_additive() {
        let _guard = scrub_settings();
        add_scrub_fields(["account_number"]);
        let payload = scrub_payload(json!({
            "context": { "accountNumber": "12345678", "orderId": "A-1024" }
        }))
        .expect("payload");

        assert_eq!(payload["context"]["accountNumber"], json!(REDACTED));
        assert_eq!(payload["context"]["orderId"], json!("A-1024"));
    }

    #[test]
    fn strips_secrets_that_leaked_into_free_text() {
        assert_eq!(
            scrub_text("401 from billing: Authorization: Bearer sk_live_abc123 rejected"),
            "401 from billing: Authorization: [redacted] rejected"
        );
        assert_eq!(
            scrub_text("token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.7Hk2 expired"),
            "token [redacted] expired"
        );
        assert_eq!(
            scrub_text("no account for ada@example.com"),
            "no account for [redacted]"
        );
    }

    #[test]
    fn strips_card_numbers_but_keeps_order_numbers() {
        let scrubbed = scrub_text("charge 4242 4242 4242 4242 failed for order 1234567890123");
        assert!(!scrubbed.contains("4242"), "{scrubbed}");
        assert!(scrubbed.contains("1234567890123"), "{scrubbed}");
    }

    #[test]
    fn keeps_user_identity_but_not_user_credentials() {
        let _guard = scrub_settings();
        let payload = scrub_payload(json!({
            "message": "no account for ada@example.com",
            "user": { "id": "u_1", "email": "ada@example.com", "session_token": "st_1" },
        }))
        .expect("payload");

        assert_eq!(payload["message"], json!("no account for [redacted]"));
        assert_eq!(payload["user"]["email"], json!("ada@example.com"));
        assert_eq!(payload["user"]["session_token"], json!(REDACTED));
    }

    #[test]
    fn before_send_can_edit_or_drop_a_payload() {
        let _guard = scrub_settings();
        set_before_send(|mut payload| {
            if payload["message"] == json!("noise") {
                return None;
            }
            payload["context"] = json!({ "note": "call ada@example.com" });
            Some(payload)
        });

        assert!(scrub_payload(json!({ "message": "noise" })).is_none());
        let payload = scrub_payload(json!({ "message": "signal" })).expect("payload");
        // Redaction runs after the hook, so a hook cannot leak a secret.
        assert_eq!(payload["context"]["note"], json!("call [redacted]"));

        clear_before_send();
        assert!(scrub_payload(json!({ "message": "noise" })).is_some());
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
