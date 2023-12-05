use axum::{
    http::{header::HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use getopts::Options;
use octocrab::{
    models::{
        pulls::ReviewState,
        webhook_events::{
            payload::{PullRequestWebhookEventAction, WebhookEventPayload},
            EventInstallation, WebhookEvent, WebhookEventType,
        },
        InstallationToken,
    },
    Octocrab,
};
use serde::Deserialize;
use tokio::signal;
use tracing::{debug, error, Instrument};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use chetter_app::{bookmark_pr, close_pr, open_pr, synchronize_pr};

#[derive(Clone, Debug)]
struct AppState {
    oc: Octocrab,
}

#[derive(Deserialize, Debug)]
struct Config {
    app_id: u64,
    private_key: String,
}

async fn installation_client(
    oc: &Octocrab,
    installation: &Option<EventInstallation>,
) -> Result<Octocrab, ()> {
    let id = match installation.as_ref() {
        Some(EventInstallation::Minimal(v)) => v.id.0,
        Some(EventInstallation::Full(v)) => v.id.0,
        None => {
            error!("missing event.installation.id");
            return Err(());
        }
    };
    let url = format!("/app/installations/{}/access_tokens", id);
    let token: InstallationToken = match oc.post(url, None::<&()>).await {
        Ok(v) => v,
        Err(octocrab::Error::GitHub { source, .. }) => {
            error!("failed to get access_token for {}: {}", id, &source.message);
            return Err(());
        }
        Err(error) => {
            error!("failed to get access_token for {}: {:?}", id, &error);
            return Err(());
        }
    };
    match octocrab::OctocrabBuilder::new()
        .personal_token(token.token)
        .build()
    {
        Ok(v) => Ok(v),
        Err(error) => {
            error!("failed to build installation client: {:?}", &error);
            Err(())
        }
    }
}

async fn handle_pull_request_review(oc: &Octocrab, ev: &WebhookEvent) -> Result<(), ()> {
    let Some(repo) = ev.repository.as_ref() else {
        error!("Missing .repository");
        return Err(());
    };

    let Some(owner) = repo.owner.as_ref() else {
        error!("{}: Missing .repository.owner", &repo.name);
        return Err(());
    };

    let WebhookEventPayload::PullRequestReview(ref payload) = ev.specific else {
        error!(
            "{}/{}: Unexpected payload: {:?}",
            &owner.login, &repo.name, &ev.specific
        );
        return Err(());
    };

    let Some(reviewer) = payload.review.user.as_ref() else {
        error!("{}/{}: Missing .review.user", &owner.login, &repo.name);
        return Err(());
    };

    let span = tracing::span!(
        tracing::Level::WARN,
        "review",
        repo = format!("{}/{}", &owner.login, &repo.name),
        pr = payload.pull_request.number,
        reviewer = reviewer.login
    );

    async move {
        let Ok(client) = installation_client(oc, &ev.installation).await else {
            return Err(());
        };

        let Some(ref sha) = payload.review.commit_id else {
            error!("missing .review.commit_id");
            return Err(());
        };

        let ret = match payload.review.state {
            Some(ReviewState::Approved | ReviewState::ChangesRequested) => {
                bookmark_pr(
                    &client,
                    &owner.login,
                    &repo.name,
                    payload.pull_request.number,
                    &reviewer.login,
                    sha,
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

async fn handle_pull_request(oc: &Octocrab, ev: &WebhookEvent) -> Result<(), ()> {
    let Some(repo) = ev.repository.as_ref() else {
        error!("Missing .repository");
        return Err(());
    };

    let Some(owner) = repo.owner.as_ref() else {
        error!("{}: Missing .repository.owner", &repo.name);
        return Err(());
    };

    let WebhookEventPayload::PullRequest(ref pr) = ev.specific else {
        error!(
            "{}/{}: Unexpected payload: {:?}",
            &owner.login, &repo.name, &ev.specific
        );
        return Err(());
    };

    let span = tracing::span!(
        tracing::Level::WARN,
        "pr",
        repo = format!("{}/{}", &owner.login, &repo.name),
        pr = pr.number
    );
    async move {
        let Ok(client) = installation_client(oc, &ev.installation).await else {
            return Err(());
        };

        let ret = match pr.action {
            PullRequestWebhookEventAction::Synchronize => {
                let sub_span = tracing::span!(tracing::Level::INFO, "synchronize");
                async move {
                    synchronize_pr(
                        &client,
                        &owner.login,
                        &repo.name,
                        pr.number,
                        &pr.pull_request.head.sha,
                    )
                    .await
                }
                .instrument(sub_span)
                .await
            }
            PullRequestWebhookEventAction::Opened => {
                let sub_span = tracing::span!(tracing::Level::INFO, "open");
                async move {
                    open_pr(
                        &client,
                        &owner.login,
                        &repo.name,
                        pr.number,
                        &pr.pull_request.head.sha,
                    )
                    .await
                }
                .instrument(sub_span)
                .await
            }
            PullRequestWebhookEventAction::Closed => {
                let sub_span = tracing::span!(tracing::Level::INFO, "close");
                async move { close_pr(&client, &owner.login, &repo.name, pr.number).await }
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
    axum::extract::State(state): axum::extract::State<AppState>,
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
        WebhookEventType::PullRequest => handle_pull_request(&state.oc, &event).await.is_ok(),
        WebhookEventType::PullRequestReview => {
            handle_pull_request_review(&state.oc, &event).await.is_ok()
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

    let config_str = std::fs::read_to_string(&config_path).unwrap_or_else(|err| {
        eprintln!("Failed to read '{}': {}", &config_path, &err);
        std::process::exit(1);
    });

    let config: Config = toml::from_str(&config_str).unwrap();
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(config.private_key.as_bytes())
        .unwrap_or_else(|err| {
            eprintln!(
                "Failed to parse `private_key` in {}: {}",
                &config_path, &err
            );
            std::process::exit(1);
        });

    let octocrab = Octocrab::builder()
        .app(config.app_id.into(), key)
        .build()
        .unwrap();
    let app_state = AppState { oc: octocrab };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,chetter_app=debug,axum::rejection=trace".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let app = axum::Router::new()
        .route("/github/events", post(post_github_events))
        .with_state(app_state);

    axum::Server::bind(&"0.0.0.0:3333".parse().unwrap())
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}
