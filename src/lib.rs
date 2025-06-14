use error::ChetterError;
use github::{AppClient, RepositoryClient, RepositoryController};
use octocrab::models::{
    pulls::ReviewState,
    webhook_events::{
        payload::{
            PullRequestReviewWebhookEventPayload, PullRequestWebhookEventAction,
            PullRequestWebhookEventPayload, WebhookEventPayload,
        },
        WebhookEvent,
    },
};
use std::marker::{Send, Sync};
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, Instrument};

pub mod error;
pub mod github;

/// Chetter Application state
#[derive(Clone)]
pub struct State {
    /// Github Application Client
    app_client: AppClient,

    /// Background tasks
    tasks: TaskTracker,
}

impl State {
    /// Create a new State using the specified configuration file
    pub fn new(config_path: String) -> Result<Self, String> {
        let app_client = match AppClient::new(config_path) {
            Ok(v) => v,
            Err(e) => return Err(format!("{e}")),
        };
        let tasks = TaskTracker::new();
        Ok(Self { app_client, tasks })
    }

    /// Close the application state, giving any background tasks a chance to finish.
    pub async fn close(&self) {
        if !self.tasks.is_empty() {
            use tokio::time::{timeout, Duration};

            info!("waiting for {} background tasks", self.tasks.len());
            self.tasks.close();
            if timeout(Duration::from_secs(600), self.tasks.wait())
                .await
                .is_err()
            {
                error!("Timeout waiting for background tasks to complete");
            }
        }
    }

    /// Dispatch GitHub Webhook Events
    ///
    /// Handles PullRequest and PullRequestReview events, ignores all others.
    pub async fn webhook_dispatcher(&self, event: WebhookEvent) -> Result<(), ChetterError> {
        // Early exit to astatevoid making a repo client when not necessary
        match event.specific {
            WebhookEventPayload::PullRequest(_) | WebhookEventPayload::PullRequestReview(_) => (),
            _ => return Ok(()),
        }

        let repo_client = self.app_client.repo_client(&event).await?;
        match event.specific {
            WebhookEventPayload::PullRequest(payload) => {
                let span = tracing::span!(
                    tracing::Level::WARN,
                    "pr",
                    repo = repo_client.full_name(),
                    pr = payload.number
                );
                async move { on_pull_request(repo_client, self.tasks.clone(), payload).await }
                    .instrument(span)
                    .await?;
            }
            WebhookEventPayload::PullRequestReview(payload) => {
                let Some(reviewer) = payload.review.user.as_ref() else {
                    let msg = "Missing .review.user";
                    error!(msg);
                    return Err(ChetterError::GithubParseError(msg.into()));
                };
                let login = reviewer.login.clone();

                let span = tracing::span!(
                    tracing::Level::WARN,
                    "review",
                    repo = repo_client.full_name(),
                    pr = payload.pull_request.number,
                    reviewer = login,
                );
                async move { on_pull_request_review(repo_client, &login, payload).await }
                    .instrument(span)
                    .await?;
            }
            _ => (),
        }
        Ok(())
    }
}

async fn on_pull_request(
    repo_client: RepositoryClient,
    tasks: TaskTracker,
    payload: Box<PullRequestWebhookEventPayload>,
) -> Result<(), ChetterError> {
    match payload.action {
        PullRequestWebhookEventAction::Synchronize => {
            let sub_span = tracing::span!(tracing::Level::INFO, "synchronize");
            async move {
                synchronize_pr(
                    repo_client,
                    payload.number,
                    &payload.pull_request.head.sha,
                    &payload.pull_request.base.sha,
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
                    payload.number,
                    &payload.pull_request.head.sha,
                    &payload.pull_request.base.sha,
                )
                .await
            }
            .instrument(sub_span)
            .await
        }
        PullRequestWebhookEventAction::Closed => {
            let sub_span = tracing::span!(tracing::Level::INFO, "close");

            // We can end up with a lot of references to remove.  We can do that in a single API
            // call using GraphQL, but it still takes over 10s to delete just 50 references.
            // Given that, we have no real choice but to run this task in the background and
            // report success to GitHub before it decides to hang up on us.
            tasks.spawn(
                async move { close_pr(repo_client, payload.number).await }.instrument(sub_span),
            );
            Ok(())
        }

        _ => {
            debug!("Ignoring PR action: {:?}", payload.action);
            Ok(())
        }
    }
}

async fn on_pull_request_review(
    repo_client: RepositoryClient,
    reviewer: &str,
    payload: Box<PullRequestReviewWebhookEventPayload>,
) -> Result<(), ChetterError> {
    let Some(ref sha) = payload.review.commit_id else {
        let msg = "missing .review.commit_id";
        error!(msg);
        return Err(ChetterError::GithubParseError(msg.into()));
    };

    match payload.review.state {
        Some(ReviewState::Approved | ReviewState::ChangesRequested) => {
            bookmark_pr(
                repo_client,
                payload.pull_request.number,
                reviewer,
                sha,
                &payload.pull_request.base.sha,
            )
            .await
        }
        _ => Ok(()),
    }
}

async fn open_pr(
    client: impl RepositoryController,
    pr: u64,
    sha: &str,
    base: &str,
) -> Result<(), ChetterError> {
    let mut errors: Vec<ChetterError> = vec![];

    for ref_name in ["head", "v1"] {
        for (suffix, target) in [("", sha), ("-base", base)] {
            if let Err(e) = client
                .create_ref(&format!("{}/{}{}", pr, ref_name, suffix), target)
                .await
            {
                errors.push(e);
            }
        }
    }

    match errors.pop() {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

async fn close_pr<T: RepositoryController + Sync + Send + 'static>(
    client: T,
    pr: u64,
) -> Result<(), ChetterError> {
    let refs = client.matching_refs(&format!("{}/", pr)).await?;
    client.delete_refs(&refs).await?;
    Ok(())
}

async fn synchronize_pr(
    client: impl RepositoryController,
    pr: u64,
    sha: &str,
    base: &str,
) -> Result<(), ChetterError> {
    let refs = client.matching_refs(&format!("{}/", pr)).await?;
    let mut errors: Vec<ChetterError> = vec![];

    for (name, target) in [("head", sha), ("head-base", base)] {
        let name = format!("{pr}/{name}");
        if refs.iter().any(|t| t.full_name.ends_with(&name)) {
            if let Err(e) = client.update_ref(&name, target).await {
                errors.push(e);
            }
        } else if let Err(e) = client.create_ref(&name, target).await {
            errors.push(e);
        }
    }

    let next_ref = if refs.is_empty() {
        1
    } else {
        let last_version: u32 = refs
            .iter()
            .filter_map(|t| t.full_name.split('v').next_back()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        last_version + 1
    };

    for (suffix, target) in [("", sha), ("-base", base)] {
        let name = format!("{pr}/v{next_ref}{suffix}");
        if let Err(e) = client.create_ref(&name, target).await {
            errors.push(e);
        }
    }

    match errors.pop() {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

async fn bookmark_pr(
    client: impl RepositoryController,
    pr: u64,
    reviewer: &str,
    sha: &str,
    base: &str,
) -> Result<(), ChetterError> {
    let refs = client
        .matching_refs(&format!("{}/{}", pr, reviewer))
        .await?;

    let mut errors: Vec<ChetterError> = vec![];

    for (suffix, target) in [("head", sha), ("head-base", base)] {
        let name = format!("{pr}/{reviewer}-{suffix}");
        if refs.iter().any(|t| t.full_name.ends_with(&suffix)) {
            if let Err(e) = client.update_ref(&name, target).await {
                errors.push(e);
            }
        } else if let Err(e) = client.create_ref(&name, target).await {
            errors.push(e);
        }
    }

    let next_ref = if refs.is_empty() {
        1
    } else {
        let last_version: u32 = refs
            .iter()
            .filter_map(|t| t.full_name.split('v').next_back()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        last_version + 1
    };

    for (suffix, target) in [("", sha), ("-base", base)] {
        let name = format!("{pr}/{reviewer}-v{next_ref}{suffix}");
        if let Err(e) = client.create_ref(&name, target).await {
            errors.push(e);
        }
    }

    match errors.pop() {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::*;

    use super::*;
    use crate::github::{MockRepositoryController, Ref};

    #[tokio::test]
    async fn test_open_pr() {
        let mut mock = MockRepositoryController::new();
        let sha = "abcd";
        let base = "deaf";
        let num = 1234;

        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v1")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/head")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v1-base")), eq(base))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/head-base")), eq(base))
            .returning(|_, _| Ok(()));

        let r = open_pr(mock, num, sha, base).await;
        assert!(r.is_ok())
    }

    #[tokio::test]
    async fn test_close_pr() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let refs = vec![
            format!("{num}/v1"),
            format!("{num}/v2"),
            format!("{num}/v2-base"),
            format!("{num}/head"),
            format!("{num}/head-base"),
            format!("{num}/reviewer-v1"),
            format!("{num}/reviewer-v2"),
            format!("{num}/reviewer-v2-base"),
            format!("{num}/reviewer-head"),
        ];
        let matches: Vec<Ref> = refs
            .iter()
            .map(|r| Ref {
                node_id: format!("node_{r}"),
                full_name: r.into(),
                sha: "_".into(),
            })
            .collect();
        let to_delete = matches.clone();

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/")))
            .return_once(|_| Ok(matches));
        mock.expect_delete_refs()
            .times(1)
            .with(eq(to_delete))
            .return_once(|_| Ok(()));
        let r = close_pr(mock, num).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_synchronize_pr() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let sha = "abc123";
        let base = "ba5e";

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/")))
            .returning(move |_| {
                let refs = vec![
                    format!("{num}/head"),
                    format!("{num}/head-base"),
                    format!("{num}/v4"),
                    format!("{num}/v4-base"),
                    format!("{num}/reviewer-v2"),
                    format!("{num}/nick-v99-head"),
                    format!("{num}/junk"),
                ];

                Ok(refs
                    .into_iter()
                    .map(|r| Ref {
                        node_id: format!("node_{r}"),
                        full_name: r,
                        sha: "_".to_string(),
                    })
                    .collect())
            });
        mock.expect_update_ref()
            .times(1)
            .with(eq(format!("{num}/head")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_update_ref()
            .times(1)
            .with(eq(format!("{num}/head-base")), eq(base))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v5")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v5-base")), eq(base))
            .returning(|_, _| Ok(()));
        let r = synchronize_pr(mock, num, sha, base).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_synchronize_pr_no_head() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let sha = "abc123";
        let base = "ba5e";

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/")))
            .returning(move |_| {
                let refs = vec![
                    format!("{num}/v4"),
                    format!("{num}/v4-base"),
                    format!("{num}/reviewer-v2"),
                    format!("{num}/nick-v99-head"),
                    format!("{num}/junk"),
                ];

                Ok(refs
                    .into_iter()
                    .map(|r| Ref {
                        node_id: format!("node_{r}"),
                        full_name: r,
                        sha: "_".to_string(),
                    })
                    .collect())
            });
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/head")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/head-base")), eq(base))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v5")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v5-base")), eq(base))
            .returning(|_, _| Ok(()));
        let r = synchronize_pr(mock, num, sha, base).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_bookmark_pr() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let sha = "abc123";
        let base = "ba54";
        let user = "me";

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/{user}")))
            .returning(move |_| {
                let refs = vec![
                    format!("{num}/{user}-head"),
                    format!("{num}/{user}-head-base"),
                    format!("{num}/{user}-v2"),
                    format!("{num}/{user}-v2-base"),
                    format!("{num}/{user}-v3"),
                    format!("{num}/{user}-v3-base"),
                    format!("{num}/{user}-v99-junk"),
                ];

                Ok(refs
                    .into_iter()
                    .map(|r| Ref {
                        node_id: format!("node_{r}"),
                        full_name: r,
                        sha: "_".into(),
                    })
                    .collect())
            });
        mock.expect_update_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-head")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_update_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-head-base")), eq(base))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-v4")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-v4-base")), eq(base))
            .returning(|_, _| Ok(()));
        let r = bookmark_pr(mock, num, user, sha, base).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_bookmark_pr_no_head() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let sha = "abc123";
        let base = "ba5e";
        let user = "me";

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/{user}")))
            .returning(move |_| {
                let refs = vec![
                    format!("{num}/{user}-v3"),
                    format!("{num}/{user}-v3-base"),
                    format!("{num}/{user}-v99-junk"),
                ];

                Ok(refs
                    .into_iter()
                    .map(|r| Ref {
                        node_id: format!("node_{r}"),
                        full_name: r,
                        sha: "_".into(),
                    })
                    .collect())
            });
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-head")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-head-base")), eq(base))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-v4")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-v4-base")), eq(base))
            .returning(|_, _| Ok(()));
        let r = bookmark_pr(mock, num, user, sha, base).await;
        assert!(r.is_ok());
    }
}
