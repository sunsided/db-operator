use opentelemetry::trace::TraceId;
use tracing_subscriber::{prelude::*, EnvFilter, Registry};

///  Fetch an opentelemetry::trace::TraceId as hex through the full tracing stack
pub fn get_trace_id() -> TraceId {
    use opentelemetry::trace::TraceContextExt as _; // opentelemetry::Context -> opentelemetry::trace::Span
    use tracing_opentelemetry::OpenTelemetrySpanExt as _; // tracing::Span to opentelemetry::Context

    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
}

#[cfg(feature = "telemetry")]
async fn init_tracer() -> Result<opentelemetry::sdk::trace::Tracer, InitTracerError> {
    let otlp_endpoint = std::env::var("OPENTELEMETRY_ENDPOINT_URL")?;

    let channel = tonic::transport::Channel::from_shared(otlp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();

    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(opentelemetry_otlp::new_exporter().tonic().with_channel(channel))
        .with_trace_config(opentelemetry::sdk::trace::config().with_resource(
            opentelemetry::sdk::Resource::new(vec![opentelemetry::KeyValue::new(
                "service.name",
                "db-operator",
            )]),
        ))
        .install_batch(opentelemetry::runtime::Tokio)?;

    Ok(tracer)
}

#[cfg(feature = "telemetry")]
#[derive(Debug, thiserror::Error)]
pub enum InitTracerError {
    #[error("The environment variable OPENTELEMETRY_ENDPOINT_URL was unset")]
    NoEndpointConfigured(#[from] std::env::VarError),
    #[error(transparent)]
    TraceError(#[from] opentelemetry::trace::TraceError),
}

/// Initialize tracing
pub async fn init() {
    let logger = tracing_subscriber::fmt::layer().compact();
    let env_filter = EnvFilter::try_from_default_env()
        .or(EnvFilter::try_new("info"))
        .unwrap();

    // Setup tracing layers
    #[cfg(feature = "telemetry")]
    match init_tracer().await {
        Ok(tracer) => {
            let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
            let collector = Registry::default().with(telemetry).with(logger).with(env_filter);

            // Initialize tracing
            tracing::subscriber::set_global_default(collector).unwrap()
        }
        Err(e) => {
            eprintln!("OpenTelemetry not initialized: {e}");
        }
    }

    #[cfg(not(feature = "telemetry"))]
    {
        // Decide on layers
        let collector = Registry::default().with(logger).with(env_filter);

        // Initialize tracing
        tracing::subscriber::set_global_default(collector).unwrap()
    }
}

#[cfg(test)]
mod test {
    // This test only works when telemetry is initialized fully
    // and requires OPENTELEMETRY_ENDPOINT_URL pointing to a valid server
    #[cfg(feature = "telemetry")]
    #[tokio::test]
    #[ignore = "requires a trace exporter"]
    async fn get_trace_id_returns_valid_traces() {
        use super::*;
        super::init().await;
        #[tracing::instrument(name = "test_span")] // need to be in an instrumented fn
        fn test_trace_id() -> TraceId {
            get_trace_id()
        }
        assert_ne!(test_trace_id(), TraceId::INVALID, "valid trace");
    }
}
