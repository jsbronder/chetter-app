use axum::{
    http::{header::HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use octocrab::{
    models::{
        repos::Ref,
        webhook_events::{
            payload::{PullRequestWebhookEventAction, WebhookEventPayload},
            EventInstallation, WebhookEvent, WebhookEventType,
        },
        InstallationToken,
    },
    params::repos::Reference,
    Octocrab,
};
use tracing::{debug, error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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

    let Some(org_repo) = repo.full_name.as_ref() else {
        error!("Missing .repository.full_name");
        return Err(());
    };

    let Some(owner) = repo.owner.as_ref() else {
        error!("{}: Missing .repository.owner", &org_repo);
        return Err(());
    };

    info!("{}: pull-request", &org_repo);

    let WebhookEventPayload::PullRequest(ref pr) = ev.specific else {
        error!("{}: Failed to parse PullRequest", &org_repo);
        return Err(());
    };

    if pr.action != PullRequestWebhookEventAction::Synchronize {
        info!("{}: Ignoring PR action: {:?}", &org_repo, pr.action);
        return Ok(());
    }

    let id = match ev.installation.as_ref() {
        Some(EventInstallation::Minimal(v)) => v.id,
        Some(EventInstallation::Full(v)) => v.id,
        None => {
            error!("{}: missing event.installation.id", &org_repo);
            return Err(());
        }
    };

    let client = match installation_client(oc, id.0).await {
        Ok(v) => v,
        Err(error) => {
            error!(
                "{}: Failed to get installation client: {:?}",
                &org_repo, error
            );
            return Err(());
        }
    };

    let refs: Vec<Ref> = match client
        .get(
            format!(
                "/repos/{}/git/matching-refs/chetter/{}/v",
                &org_repo, pr.number
            ),
            None::<&()>,
        )
        .await
    {
        Ok(v) => v,
        Err(octocrab::Error::GitHub { source, .. }) => {
            error!("{}: github: {}", &org_repo, source.message);
            return Err(());
        }
        Err(error) => {
            error!("{}: failed to get pr refs: {:?}", &org_repo, error);
            return Err(());
        }
    };

    // We use Commit so that we can use a full refspec, refs/chetter/..., that won't get modified
    // by ref_url() or full_ref_url().
    let next_ref: Reference = if refs.is_empty() {
        Reference::Commit(format!("refs/chetter/{}/v1", pr.number))
    } else {
        let last_version = refs
            .iter()
            .map(|t| {
                t.ref_field
                    .split('v')
                    .last()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        Reference::Commit(format!("refs/chetter/{}/v{}", pr.number, last_version + 1))
    };

    if let Err(error) = client
        .repos(&owner.login, &repo.name)
        .create_ref(&next_ref, &pr.pull_request.head.sha)
        .await
    {
        error!("Failed to make tag: {:?}", error);
        return Err(());
    }
    info!("{}: Created {}", &org_repo, &next_ref);

    Ok(())
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
