use std::sync::Arc;

use anyhow::{Context, Result};
use opentelemetry::trace::{Span, Tracer};
use opentelemetry::{KeyValue, trace::TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;

use super::{Observer, TraceEvent};

pub(crate) trait OtelEmitter: Send + Sync {
    fn emit(&self, event: &TraceEvent) -> Result<()>;
}

struct SdkOtelEmitter {
    tracer: opentelemetry_sdk::trace::Tracer,
    _provider: opentelemetry_sdk::trace::TracerProvider,
}

impl SdkOtelEmitter {
    fn new(endpoint: String) -> Result<Self> {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("failed to build otlp span exporter")?;
        let provider = opentelemetry_sdk::trace::TracerProvider::builder()
            .with_simple_exporter(exporter)
            .build();
        let tracer = provider.tracer("autopoiesis");
        Ok(Self {
            tracer,
            _provider: provider,
        })
    }
}

impl OtelEmitter for SdkOtelEmitter {
    fn emit(&self, event: &TraceEvent) -> Result<()> {
        let mut span = self.tracer.start(event.event_type().to_string());
        span.set_attribute(KeyValue::new("event_type", event.event_type()));
        if let Some(session_id) = event.session_id() {
            span.set_attribute(KeyValue::new("session_id", session_id.to_string()));
        }
        if let Some(turn_id) = event.turn_id() {
            span.set_attribute(KeyValue::new("turn_id", turn_id.to_string()));
        }
        if let Some(plan_run_id) = event.plan_run_id() {
            span.set_attribute(KeyValue::new("plan_run_id", plan_run_id.to_string()));
        }
        if let Some(eval_run_id) = event.eval_run_id() {
            span.set_attribute(KeyValue::new("eval_run_id", eval_run_id.to_string()));
        }
        span.end();
        Ok(())
    }
}

pub struct OtelObserver {
    emitter: Option<Arc<dyn OtelEmitter>>,
}

impl OtelObserver {
    pub fn new() -> Result<Self> {
        let endpoint = std::env::var("ZO_OTEL_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:5081".to_string());
        Self::new_with_endpoint(endpoint)
    }

    pub fn new_with_endpoint(endpoint: String) -> Result<Self> {
        let emitter = SdkOtelEmitter::new(endpoint)?;
        Ok(Self {
            emitter: Some(Arc::new(emitter)),
        })
    }

    #[cfg(test)]
    #[cfg(all(test, not(clippy)))]
    pub(crate) fn with_emitter(emitter: Arc<dyn OtelEmitter>) -> Self {
        Self {
            emitter: Some(emitter),
        }
    }
}

impl Observer for OtelObserver {
    fn emit(&self, event: &TraceEvent) {
        if let Some(emitter) = &self.emitter
            && let Err(error) = emitter.emit(event)
        {
            tracing::warn!(%error, "failed to emit otel trace event");
        }
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingEmitter {
        count: AtomicUsize,
    }

    impl RecordingEmitter {
        fn new() -> Self {
            Self {
                count: AtomicUsize::new(0),
            }
        }
    }

    impl OtelEmitter for RecordingEmitter {
        fn emit(&self, _event: &TraceEvent) -> Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FailingEmitter;

    impl OtelEmitter for FailingEmitter {
        fn emit(&self, _event: &TraceEvent) -> Result<()> {
            Err(anyhow::anyhow!("forced otel failure"))
        }
    }

    #[test]
    fn emits_via_injected_emitter() {
        let emitter = Arc::new(RecordingEmitter::new());
        let observer = OtelObserver::with_emitter(emitter.clone());
        observer.emit(&TraceEvent::EvalRunStarted {
            eval_run_id: "eval-1".to_string(),
            session_id: None,
        });
        assert_eq!(emitter.count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn emit_failure_is_swallowed() {
        let observer = OtelObserver::with_emitter(Arc::new(FailingEmitter));
        observer.emit(&TraceEvent::EvalRunStarted {
            eval_run_id: "eval-1".to_string(),
            session_id: None,
        });
    }
}
