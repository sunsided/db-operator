#![allow(unused_imports, unused_variables)]

use actix_web::{get, middleware, web::Data, App, HttpRequest, HttpResponse, HttpServer, Responder};
pub use controller::{self, telemetry, State};
use prometheus::{Encoder, TextEncoder};
use std::env;
use structured_logger::async_json::new_writer;
use structured_logger::Builder;

#[get("/metrics")]
async fn metrics(c: Data<State>, _req: HttpRequest) -> impl Responder {
    let metrics = c.metrics();
    let encoder = TextEncoder::new();
    let mut buffer = vec![];
    encoder.encode(&metrics, &mut buffer).unwrap();
    HttpResponse::Ok().body(buffer)
}

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
}

#[get("/")]
async fn index(c: Data<State>, _req: HttpRequest) -> impl Responder {
    let d = c.diagnostics().await;
    HttpResponse::Ok().json(&d)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // init_logging();
    telemetry::init().await;

    // Initialize Kubernetes controller state
    let state = State::default();
    let controller = controller::run(state.clone());

    // Start web server
    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(state.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
            .service(metrics)
    })
    .bind("0.0.0.0:8080")?
    .shutdown_timeout(5);

    // Both runtimes implements graceful shutdown, so poll until both are done
    tokio::join!(controller, server.run()).1?;
    Ok(())
}

#[allow(dead_code)]
fn init_logging() {
    let log_level = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    Builder::with_level("info")
        .with_target_writer("*", new_writer(tokio::io::stdout()))
        .init();
}
