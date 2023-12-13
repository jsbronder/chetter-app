use axum::{
    http::{header::HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use getopts::Options;
use octocrab::models::{
    pulls::ReviewState,
    webhook_events::{
        payload::{PullRequestWebhookEventAction, WebhookEventPayload},
        WebhookEvent, WebhookEventType,
    },
};
use tokio::signal;
use tracing::{debug, error, Instrument};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use chetter_app::{bookmark_pr, close_pr, github::AppClient, open_pr, synchronize_pr};

async fn handle_pull_request_review(app_client: AppClient, ev: WebhookEvent) -> Result<(), ()> {
    let Ok(repo_client) = app_client.repo_client(&ev).await else {
        return Err(());
    };

    let WebhookEventPayload::PullRequestReview(ref payload) = ev.specific else {
        error!("Unexpected payload: {:?}", &ev.specific);
        return Err(());
    };

    let Some(reviewer) = payload.review.user.as_ref() else {
        error!("Missing .review.user");
        return Err(());
    };

    let span = tracing::span!(
        tracing::Level::WARN,
        "review",
        repo = repo_client.full_name(),
        pr = payload.pull_request.number,
        reviewer = reviewer.login
    );

    async move {
        let Some(ref sha) = payload.review.commit_id else {
            error!("missing .review.commit_id");
            return Err(());
        };

        let ret = match payload.review.state {
            Some(ReviewState::Approved | ReviewState::ChangesRequested) => {
                bookmark_pr(
                    repo_client,
                    payload.pull_request.number,
                    &reviewer.login,
                    sha,
                    &payload.pull_request.base.sha,
                )
                .await
            }
            _ => Ok(()),
        };

        if ret.is_ok() {
            Ok(())
        } else {
            Err(())
        }
    }
    .instrument(span)
    .await
}

async fn handle_pull_request(app_client: AppClient, ev: WebhookEvent) -> Result<(), ()> {
    let Ok(repo_client) = app_client.repo_client(&ev).await else {
        return Err(());
    };

    let WebhookEventPayload::PullRequest(ref pr) = ev.specific else {
        error!("Unexpected payload: {:?}", &ev.specific);
        return Err(());
    };

    let span = tracing::span!(
        tracing::Level::WARN,
        "pr",
        repo = repo_client.full_name(),
        pr = pr.number
    );
    async move {
        let ret = match pr.action {
            PullRequestWebhookEventAction::Synchronize => {
                let sub_span = tracing::span!(tracing::Level::INFO, "synchronize");
                async move {
                    synchronize_pr(
                        repo_client,
                        pr.number,
                        &pr.pull_request.head.sha,
                        &pr.pull_request.base.sha,
                    )
                    .await
                }
                .instrument(sub_span)
                .await
            }
            PullRequestWebhookEventAction::Opened | PullRequestWebhookEventAction::Reopened => {
                let sub_span = tracing::span!(tracing::Level::INFO, "open");
                async move {
                    open_pr(
                        repo_client,
                        pr.number,
                        &pr.pull_request.head.sha,
                        &pr.pull_request.base.sha,
                    )
                    .await
                }
                .instrument(sub_span)
                .await
            }
            PullRequestWebhookEventAction::Closed => {
                let sub_span = tracing::span!(tracing::Level::INFO, "close");
                async move { close_pr(repo_client, pr.number).await }
                    .instrument(sub_span)
                    .await
            }

            _ => {
                debug!("Ignoring PR action: {:?}", pr.action);
                Ok(())
            }
        };

        if ret.is_ok() {
            Ok(())
        } else {
            error!("failed to handle {:?}", pr.action);
            Err(())
        }
    }
    .instrument(span)
    .await
}

async fn post_github_events(
    axum::extract::State(app_client): axum::extract::State<AppClient>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let event_type = match headers.get("X-Github-Event") {
        Some(v) => match v.to_str() {
            Ok(v) => v,
            Err(error) => {
                error!("Failed to parse X-Github-Event: {}", error);
                headers.iter().for_each(|(k, v)| {
                    debug!("{} = {}", k, v.to_str().unwrap_or("<error>"));
                });
                return (
                    StatusCode::BAD_REQUEST,
                    format!("Failed to parse X-Github-Event: {}", error),
                );
            }
        },
        None => {
            let msg = "No X-Github-Event header";
            error!(msg);
            headers.iter().for_each(|(k, v)| {
                debug!("{} = {}", k, v.to_str().unwrap_or("<error>"));
            });
            return (StatusCode::BAD_REQUEST, msg.into());
        }
    };

    let event = match WebhookEvent::try_from_header_and_body(event_type, &body) {
        Ok(event) => event,
        Err(error) => {
            let msg = format!("Failed to parse event: {}", error);
            error!(msg);
            debug!("{}", body);
            return (StatusCode::BAD_REQUEST, msg);
        }
    };

    let ret = match event.kind {
        WebhookEventType::PullRequest => handle_pull_request(app_client, event).await.is_ok(),
        WebhookEventType::PullRequestReview => {
            handle_pull_request_review(app_client, event).await.is_ok()
        }
        _ => true,
    };

    if ret {
        (StatusCode::OK, "".to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "".to_string())
    }
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

    let app_client = AppClient::new(config_path).unwrap_or_else(|err| {
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
        .with_state(app_client);

    axum::Server::bind(&"0.0.0.0:3333".parse().unwrap())
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}
