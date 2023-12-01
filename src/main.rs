use axum::{
    http::{header::HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use octocrab::{
    models::{
        webhook_events::{
            payload::{PullRequestWebhookEventAction, WebhookEventPayload},
            EventInstallation, WebhookEvent, WebhookEventType,
        },
        InstallationToken,
    },
    Octocrab,
};
use tracing::{debug, error, Instrument};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use chetter_app::{open_pr, synchronize_pr};

#[derive(Clone, Debug)]
struct AppState {
    oc: Octocrab,
}

async fn installation_client(oc: &Octocrab, id: u64) -> Result<Octocrab, octocrab::Error> {
    let url = format!("/app/installations/{}/access_tokens", id);
    let token: InstallationToken = oc.post(url, None::<&()>).await?;
    octocrab::OctocrabBuilder::new()
        .personal_token(token.token)
        .build()
}

async fn handle_github_event(oc: &Octocrab, ev: &WebhookEvent) -> Result<(), ()> {
    if ev.kind != WebhookEventType::PullRequest {
        error!("Unexpected webhook event: {:?}", ev.kind);
        return Err(());
    }

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
        let id = match ev.installation.as_ref() {
            Some(EventInstallation::Minimal(v)) => v.id,
            Some(EventInstallation::Full(v)) => v.id,
            None => {
                error!("missing event.installation.id");
                return Err(());
            }
        };

        let client = match installation_client(oc, id.0).await {
            Ok(v) => v,
            Err(error) => {
                error!("Failed to get installation client: {:?}", error);
                return Err(());
            }
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

    if handle_github_event(&state.oc, &event).await.is_ok() {
        (StatusCode::OK, "".to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "".to_string())
    }
}

fn getenv(var: &str) -> String {
    let err = format!("Missing environment variable: {var}");
    std::env::var(var).expect(&err)
}

#[tokio::main]
async fn main() {
    let app_id = getenv("GH_APP_ID").parse::<u64>().unwrap().into();
    let key_str = std::fs::read_to_string(getenv("GH_APP_PEM")).expect("Failed to read GH_APP_PEM");
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(key_str.as_bytes()).unwrap();
    let octocrab = Octocrab::builder().app(app_id, key).build().unwrap();
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
        .await
        .unwrap();
}
