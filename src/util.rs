use std::fmt;

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::{FormatEvent, Writer};
use tracing_subscriber::registry::LookupSpan;

pub const STDOUT_USER_OUTPUT_TARGET: &str = "autopoiesis.stdout";
pub const STDERR_USER_OUTPUT_TARGET: &str = "autopoiesis.stderr";

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }
}

pub struct PlainMessageFormatter;

impl<S, N> FormatEvent<S, N> for PlainMessageFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        if let Some(message) = visitor.message {
            write!(writer, "{message}")?;
        }
        writeln!(writer)
    }
}

pub(crate) use crate::time::utc_timestamp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracing_targets_are_distinct_and_nonempty() {
        assert!(!STDOUT_USER_OUTPUT_TARGET.is_empty());
        assert!(!STDERR_USER_OUTPUT_TARGET.is_empty());
        assert_ne!(STDOUT_USER_OUTPUT_TARGET, STDERR_USER_OUTPUT_TARGET);
    }

    #[test]
    fn plain_message_formatter_keeps_only_message_text() {
        use std::io::{self, Write};
        use std::sync::{Arc, Mutex};

        use tracing_subscriber::fmt;

        #[derive(Clone)]
        struct SharedWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for SharedWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("writer lock").extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = fmt()
            .event_format(PlainMessageFormatter)
            .with_writer({
                let output = output.clone();
                move || SharedWriter(output.clone())
            })
            .finish();

        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::info!(target: STDOUT_USER_OUTPUT_TARGET, "hello");

        let rendered = output.lock().expect("writer lock").clone();
        assert_eq!(String::from_utf8(rendered).expect("utf8"), "hello\n");
    }
}
