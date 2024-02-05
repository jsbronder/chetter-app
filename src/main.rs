use axum::{http::header::HeaderMap, routing::post};
use getopts::Options;
use octocrab::models::webhook_events::WebhookEvent;
use tokio::signal;
use tracing::{debug, error};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use chetter_app::{error::ChetterError, webhook_dispatcher, State};

async fn post_github_events(
    axum::extract::State(state): axum::extract::State<State>,
    headers: HeaderMap,
    body: String,
) -> Result<(), ChetterError> {
    let event_type = match headers.get("X-Github-Event") {
        Some(v) => match v.to_str() {
            Ok(v) => v,
            Err(error) => {
                error!("Failed to parse X-Github-Event: {}", error);
                headers.iter().for_each(|(k, v)| {
                    debug!("{} = {}", k, v.to_str().unwrap_or("<error>"));
                });
                return Err(ChetterError::GithubParseError(format!(
                    "Failed to parse X-Github-Event: {error}"
                )));
            }
        },
        None => {
            let msg = "No X-Github-Event header";
            error!(msg);
            headers.iter().for_each(|(k, v)| {
                debug!("{} = {}", k, v.to_str().unwrap_or("<error>"));
            });
            return Err(ChetterError::GithubParseError(msg.into()));
        }
    };

    let event = match WebhookEvent::try_from_header_and_body(event_type, &body) {
        Ok(event) => event,
        Err(error) => {
            let msg = format!("Failed to parse event: {}", error);
            error!(msg);
            debug!("{}", body);
            return Err(ChetterError::GithubParseError(msg));
        }
    };

    webhook_dispatcher(state, event).await
}

async fn shutdown_signal() {
    let sigint = async {
        signal::ctrl_c().await.unwrap_or_else(|err| {
            panic!("failed to install SIGINT handler: {}", err);
        });
    };

    let sigterm = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .unwrap_or_else(|err| {
                panic!("failed to install SIGINT handler: {}", err);
            })
            .recv()
            .await;
    };

    tokio::select! {
        _ = sigint => {println!("shutdown due to sigint")},
        _ = sigterm => {println!("shutdown due to sigterm")},
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut opts = Options::new();
    opts.optflag("h", "help", "print this help menu");
    opts.optopt("c", "config", "path to config file", "FILE");
    let matches = opts.parse(&args[1..]).unwrap_or_else(|err| {
        eprintln!("Failed to parse commandline arguments: {}", &err);
        std::process::exit(1);
    });

    if matches.opt_present("h") {
        println!("{}", opts.usage("Usage: chetter-app [OPTIONS]"));
        std::process::exit(0);
    }

    let Some(config_path) = matches.opt_str("c") else {
        eprintln!("Error: config file (-c,--config) required");
        std::process::exit(1);
    };

    let state = State::new(config_path).unwrap_or_else(|err| {
        eprintln!("{}", err);
        std::process::exit(1);
    });

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,chetter_app=debug,axum::rejection=trace".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let app = axum::Router::new()
        .route("/github/events", post(post_github_events))
        .with_state(state);

    axum::Server::bind(&"0.0.0.0:3333".parse().unwrap())
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}
