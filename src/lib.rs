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
use tracing::{debug, error, Instrument};

pub mod error;
pub mod github;

/// Dispatch GitHub Webhook Events
///
/// Handles PullRequest and PullRequestReview events, ignores all others.
pub async fn webhook_dispatcher(
    app_client: AppClient,
    event: WebhookEvent,
) -> Result<(), ChetterError> {
    // Early exit to avoid making a repo client when not necessary
    match event.specific {
        WebhookEventPayload::PullRequest(_) | WebhookEventPayload::PullRequestReview(_) => (),
        _ => return Ok(()),
    }

    let repo_client = app_client.repo_client(&event).await?;
    match event.specific {
        WebhookEventPayload::PullRequest(payload) => {
            let span = tracing::span!(
                tracing::Level::WARN,
                "pr",
                repo = repo_client.full_name(),
                pr = payload.number
            );
            async move { on_pull_request(repo_client, payload).await }
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

async fn on_pull_request(
    repo_client: RepositoryClient,
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
            async move { close_pr(repo_client, payload.number).await }
                .instrument(sub_span)
                .await
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
    let refs: Vec<String> = client
        .matching_refs(&format!("{}/", pr))
        .await?
        .iter()
        .map(|ref_obj| ref_obj.full_name.replace("refs/chetter/", ""))
        .collect();

    let client = std::sync::Arc::new(client);

    let mut set = tokio::task::JoinSet::new();
    for ref_name in refs {
        let client = client.clone();
        set.spawn(async move { client.delete_ref(&ref_name).await });
    }

    let mut errors: Vec<ChetterError> = vec![];
    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(_)) => (),
            Ok(Err(e)) => errors.push(e),
            Err(e) => errors.push(e.into()),
        }
    }

    match errors.pop() {
        None => Ok(()),
        Some(e) => Err(e),
    }
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
            .filter_map(|t| t.full_name.split('v').last()?.parse::<u32>().ok())
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
            .filter_map(|t| t.full_name.split('v').last()?.parse::<u32>().ok())
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
        let matches = refs
            .iter()
            .map(|r| Ref {
                full_name: format!("refs/chetter/{r}"),
                sha: "_".into(),
            })
            .collect();

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/")))
            .return_once(|_| Ok(matches));
        refs.iter().for_each(|r| {
            mock.expect_delete_ref()
                .times(1)
                .with(eq(r.to_string()))
                .return_once(|_| Ok(()));
        });
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
                    format!("refs/chetter/{num}/head"),
                    format!("refs/chetter/{num}/head-base"),
                    format!("refs/chetter/{num}/v4"),
                    format!("refs/chetter/{num}/v4-base"),
                    format!("refs/chetter/{num}/reviewer-v2"),
                    format!("refs/chetter/{num}/nick-v99-head"),
                    format!("refs/chetter/{num}/junk"),
                ];

                Ok(refs
                    .iter()
                    .map(|r| Ref {
                        full_name: r.into(),
                        sha: "_".into(),
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
                    format!("refs/chetter/{num}/v4"),
                    format!("refs/chetter/{num}/v4-base"),
                    format!("refs/chetter/{num}/reviewer-v2"),
                    format!("refs/chetter/{num}/nick-v99-head"),
                    format!("refs/chetter/{num}/junk"),
                ];

                Ok(refs
                    .iter()
                    .map(|r| Ref {
                        full_name: r.into(),
                        sha: "_".into(),
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
                    format!("refs/chetter/{num}/{user}-head"),
                    format!("refs/chetter/{num}/{user}-head-base"),
                    format!("refs/chetter/{num}/{user}-v2"),
                    format!("refs/chetter/{num}/{user}-v2-base"),
                    format!("refs/chetter/{num}/{user}-v3"),
                    format!("refs/chetter/{num}/{user}-v3-base"),
                    format!("refs/chetter/{num}/{user}-v99-junk"),
                ];

                Ok(refs
                    .iter()
                    .map(|r| Ref {
                        full_name: r.into(),
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
                    format!("refs/chetter/{num}/{user}-v3"),
                    format!("refs/chetter/{num}/{user}-v3-base"),
                    format!("refs/chetter/{num}/{user}-v99-junk"),
                ];

                Ok(refs
                    .iter()
                    .map(|r| Ref {
                        full_name: r.into(),
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
