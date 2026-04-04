use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::prelude::*;

use autopoiesis::logging::{
    PlainMessageFormatter, STDERR_USER_OUTPUT_TARGET, STDOUT_USER_OUTPUT_TARGET,
};

fn build_tracing_subscriber_with_filters<DW, SW, EW>(
    diagnostic_writer: DW,
    stdout_writer: SW,
    stderr_writer: EW,
    diagnostic_filter: EnvFilter,
    stdout_filter: EnvFilter,
    stderr_filter: EnvFilter,
) -> impl tracing::Subscriber + Send + Sync
where
    DW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    SW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    EW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    // Invariant: these are static format strings — parsing cannot fail.
    #[allow(clippy::expect_used)]
    let diagnostic_filter = diagnostic_filter
        .add_directive(
            format!("{STDOUT_USER_OUTPUT_TARGET}=off")
                .parse()
                .expect("stdout user-output target directive should parse"),
        )
        .add_directive(
            format!("{STDERR_USER_OUTPUT_TARGET}=off")
                .parse()
                .expect("stderr user-output target directive should parse"),
        );

    let diagnostic_layer = tracing_subscriber::fmt::layer()
        .with_writer(diagnostic_writer)
        .with_filter(diagnostic_filter);
    let stdout_layer = tracing_subscriber::fmt::layer()
        .event_format(PlainMessageFormatter)
        .with_writer(stdout_writer)
        .with_ansi(false)
        .with_filter(stdout_filter);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .event_format(PlainMessageFormatter)
        .with_writer(stderr_writer)
        .with_ansi(false)
        .with_filter(stderr_filter);

    tracing_subscriber::registry()
        .with(diagnostic_layer)
        .with(stdout_layer)
        .with(stderr_layer)
}

fn build_tracing_subscriber<DW, SW, EW>(
    diagnostic_writer: DW,
    stdout_writer: SW,
    stderr_writer: EW,
) -> impl tracing::Subscriber + Send + Sync
where
    DW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    SW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    EW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let diagnostic_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let stdout_filter = EnvFilter::new(format!("{STDOUT_USER_OUTPUT_TARGET}=trace"));
    let stderr_filter = EnvFilter::new(format!("{STDERR_USER_OUTPUT_TARGET}=trace"));

    build_tracing_subscriber_with_filters(
        diagnostic_writer,
        stdout_writer,
        stderr_writer,
        diagnostic_filter,
        stdout_filter,
        stderr_filter,
    )
}

pub(crate) fn init_tracing() {
    let subscriber = build_tracing_subscriber(std::io::stderr, std::io::stdout, std::io::stderr);
    let _ = subscriber.try_init();
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

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

    #[test]
    fn tracing_layers_route_user_output_without_duplication() {
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let diagnostic = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let subscriber = build_tracing_subscriber_with_filters(
            {
                let diagnostic = diagnostic.clone();
                move || SharedWriter(diagnostic.clone())
            },
            {
                let stdout = stdout.clone();
                move || SharedWriter(stdout.clone())
            },
            {
                let stderr = stderr.clone();
                move || SharedWriter(stderr.clone())
            },
            EnvFilter::new("info"),
            EnvFilter::new(format!("{STDOUT_USER_OUTPUT_TARGET}=trace")),
            EnvFilter::new(format!("{STDERR_USER_OUTPUT_TARGET}=trace")),
        );

        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::info!(target: STDOUT_USER_OUTPUT_TARGET, "hello");
        tracing::warn!("diagnostic");
        tracing::info!(target: STDERR_USER_OUTPUT_TARGET, "denial");

        let diagnostic_text =
            String::from_utf8(diagnostic.lock().expect("diagnostic lock").clone())
                .expect("diagnostic utf8");
        let stdout_text =
            String::from_utf8(stdout.lock().expect("stdout lock").clone()).expect("stdout utf8");
        let stderr_text =
            String::from_utf8(stderr.lock().expect("stderr lock").clone()).expect("stderr utf8");

        assert_eq!(stdout_text, "hello\n");
        assert_eq!(diagnostic_text.matches("diagnostic").count(), 1);
        assert_eq!(stderr_text.matches("denial\n").count(), 1);
        assert!(!diagnostic_text.contains("hello\n"));
        assert!(!stderr_text.contains("hello\n"));
        assert!(!diagnostic_text.contains("denial\n"));
        assert!(!stderr_text.contains("hello\n"));
    }
}
