# octri-monitoring (Rust)

**Error and performance monitoring for Rust services.** Report a panic or a
returned error with a symbolic backtrace, emit your own application events, time
spans into a request waterfall, and continue a W3C distributed trace that started
in whichever client called you.

Octri turns an OpenAPI spec into a documentation site, client SDKs for ten
languages, an MCP server your AI assistant can call, and monitoring for the
API behind them. This crate is the Rust monitoring runtime, and it works on
its own: a generated Octri API SDK is not required. See
[octri.dev/monitoring](https://octri.dev/monitoring).

Rust 1.70 or newer.

## Install

```bash
cargo add octri-monitoring
```

## Setup

Call `init` once at startup.

```rust
use octri_monitoring::{init, Config};

init(Config {
    url: "https://monitoring.example.com".into(), // your monitoring base URL
    token: std::env::var("OCTRI_TOKEN").ok(),     // your project ingest token
    environment: "<your project id>".into(),      // the dashboard project id
    release: std::env::var("GIT_SHA").ok(),       // optional
});
```

Hosted users copy the project-scoped URL, token, and environment from the
Monitoring connection settings in the dashboard. Set `token: None` only when you
point at an open self-hosted ingest endpoint.

Every call is asynchronous, best effort, and carries an idempotency key.
Transport failures are swallowed, so a monitoring outage cannot affect the
process it is watching.

## Application events

```rust
use octri_monitoring::{capture_event, EventOptions};
use std::collections::BTreeMap;

capture_event(
    "checkout.completed",
    EventOptions {
        level: Some("info".into()),
        tags: Some(BTreeMap::from([
            ("region".into(), "eu-west".into()),
            ("plan".into(), "growth".into()),
        ])),
        ..Default::default()
    },
);
```

`EventOptions` also carries `user`, `context`, `breadcrumbs`, `fingerprint`,
`operation_id`, `method`, `path`, `status_code`, `latency_ms`, `attempt`, and
`request_id`. Supplying `event_id` makes a retried delivery idempotent.

## Errors

```rust
use octri_monitoring::{capture_error, ErrorOptions};

if let Err(err) = handler(&req) {
    capture_error(&err, ErrorOptions {
        method: Some("POST".into()),
        path: Some("/orders".into()),
        status_code: Some(500),
        ..Default::default()
    });
}
```

The error is stamped with its type name, its `Display` output, and a forced
`std::backtrace::Backtrace`.

## Joining the caller's trace

Your generated client SDK sends `traceparent: 00-<traceId>-<spanId>-01` on every
request. Read it on the way in and pass the result as `trace`, and the dashboard
groups the client call and the server error under one `traceId`: the request that
failed, beside the frame that threw.

```rust
use octri_monitoring::{capture_error, trace_from_header, ErrorOptions};

let trace = trace_from_header(req.headers().get("traceparent").and_then(|v| v.to_str().ok()));

capture_error(&err, ErrorOptions { trace: Some(trace), ..Default::default() });
```

`trace_from_header(None)` starts a fresh trace, so the same code path works for
traffic that arrives without a header.

## Spans

A span is a completed unit of work with ISO-8601 timestamps. Report one per
request to get the waterfall, and one per sub-operation to see where the time
went inside it.

```rust
use octri_monitoring::{capture_span, Span};

capture_span(Span {
    trace_id: trace.trace_id.clone(),
    span_id: span_id.clone(),
    parent_span_id: trace.parent_span_id.clone(),
    name: "orders.list".into(),
    service: "server".into(),
    operation_id: Some("listOrders".into()),
    start_time: started_at,
    end_time: Some(finished_at),
    status: "ok".into(),
});
```

Spans sharing a `trace_id` nest by `parent_span_id` in the dashboard waterfall.

---

## What gets redacted

Payloads are scrubbed on the way out, so a credential that ended up in a log
line or a context object never reaches the dashboard.

Any key whose name looks like a credential (`password`, `secret`, `token`,
`apiKey`, `authorization`, `cookie`, `ssn` and the rest of the usual list) has
its value replaced with `[redacted]`, at any depth. Matching ignores case and
separators, so `api_key`, `apiKey` and `X-API-KEY` are all the same key.

Free text is swept too: the message, an error message and its stack, and
anything else you send as a string. Bearer tokens, JWTs, card numbers and email
addresses come out as `[redacted]`. A card number has to pass the Luhn check
first, so an order number or a timestamp survives.

`user` is the exception. It is the field you fill with an identity on purpose,
so `user.email` is reported exactly as you set it. Credential-shaped keys inside
it are still redacted.

Add your own key names:

```rust
octri_monitoring::add_scrub_fields(["account_number", "otp"]);
```

Or take the payload yourself, and return `None` to drop the event:

```rust
octri_monitoring::set_before_send(|payload| {
    if payload["path"] == "/health" {
        return None;
    }
    Some(payload)
});
```

Redaction runs after your hook, so a hook cannot leak a credential by accident.

## The rest of Octri

| Product | What it does |
|---|---|
| [API Studio](https://octri.dev/api-studio) | Your OpenAPI spec becomes a hosted documentation site with a live request playground, editable page by page. |
| [SDK Studio](https://octri.dev/sdk-studio) | The same spec becomes client libraries for ten languages, versioned and released together. |
| [MCP](https://octri.dev/mcp) | Your endpoints and docs become tools an AI assistant can call, generated from the same spec. |
| [Monitoring](https://octri.dev/monitoring) | Errors, traces, uptime and releases for the API, joined to the SDK calls that reached it. |

### Monitoring runtimes

- [Node](https://github.com/octridev/octri-node)
- [Python](https://github.com/octridev/octri-python)
- [Go](https://github.com/octridev/octri-go)
- [Ruby](https://github.com/octridev/octri-ruby)
- [Rust](https://github.com/octridev/octri-rust)
- [PHP](https://github.com/octridev/octri-php)
- [Java](https://github.com/octridev/octri-java)
- [Kotlin](https://github.com/octridev/octri-kotlin)
- [Swift](https://github.com/octridev/octri-swift)
- [Dart](https://github.com/octridev/octri-dart)

### More

- [Documentation](https://docs.octri.dev/docs)
- [Pricing](https://octri.dev/pricing)
- [Changelog](https://docs.octri.dev/changelog)

MIT licensed.
