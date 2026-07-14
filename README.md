# Octri Monitoring for Rust

Standalone events, error capture, W3C trace propagation, and span ingestion for
Rust 1.70+.

```rust
use octri_monitoring::{capture_event, init, Config, EventOptions};

init(Config {
    url: "https://monitoring.example.com".into(),
    token: std::env::var("OCTRI_TOKEN").ok(),
    environment: "<your project id>".into(),
    release: std::env::var("GIT_SHA").ok(),
});

capture_event("checkout.completed", EventOptions::default());
```

The project-scoped URL, token, and environment are shown in Octri's Monitoring
connection settings. Set `token: None` only for an open self-hosted endpoint.
Delivery is asynchronous, best-effort, and idempotency-keyed.
