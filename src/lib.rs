use github::RepositoryController;

pub mod error;
pub mod github;

pub async fn open_pr(client: impl RepositoryController, pr: u64, sha: &str) -> Result<(), ()> {
    let mut failed = false;
    for ref_name in &["head", "v1"] {
        if client
            .create_ref(&format!("{}/{}", pr, ref_name), sha)
            .await
            .is_err()
        {
            failed = true;
        }
    }
    if failed {
        return Err(());
    }

    Ok(())
}

pub async fn close_pr(client: impl RepositoryController, pr: u64) -> Result<(), ()> {
    let Ok(refs) = client.matching_refs(&format!("{}/", pr)).await else {
        return Err(());
    };

    let mut failed = false;
    for ref_obj in refs.iter() {
        if client
            .delete_ref(&ref_obj.full_name.replace("refs/chetter/", ""))
            .await
            .is_err()
        {
            failed = true;
        }
    }

    if failed {
        return Err(());
    }

    Ok(())
}

pub async fn synchronize_pr(
    client: impl RepositoryController,
    pr: u64,
    sha: &str,
) -> Result<(), ()> {
    let Ok(refs) = client.matching_refs(&format!("{}/", pr)).await else {
        return Err(());
    };

    if refs.iter().any(|t| t.full_name.ends_with("/head")) {
        let ref_name = format!("{}/head", pr);
        let _ = client.update_ref(&ref_name, sha).await;
    } else {
        let ref_name = format!("{}/head", pr);
        let _ = client.create_ref(&ref_name, sha).await;
    }

    let next_ref = if refs.is_empty() {
        format!("{}/v1", pr)
    } else {
        let last_version: u32 = refs
            .iter()
            .filter_map(|t| t.full_name.split('v').last()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("{}/v{}", pr, last_version + 1)
    };

    if client.create_ref(&next_ref, sha).await.is_err() {
        return Err(());
    }

    Ok(())
}

pub async fn bookmark_pr(
    client: impl RepositoryController,
    pr: u64,
    reviewer: &str,
    sha: &str,
) -> Result<(), ()> {
    let Ok(refs) = client.matching_refs(&format!("{}/{}", pr, reviewer)).await else {
        return Err(());
    };

    if refs.iter().any(|t| t.full_name.ends_with("-head")) {
        let ref_name = format!("{}/{}-head", pr, reviewer);
        let _ = client.update_ref(&ref_name, sha).await;
    } else {
        let ref_name = format!("{}/{}-head", pr, reviewer);
        let _ = client.create_ref(&ref_name, sha).await;
    }

    let next_ref = if refs.is_empty() {
        format!("{}/{}-v1", pr, reviewer)
    } else {
        let last_version: u32 = refs
            .iter()
            .filter_map(|t| t.full_name.split('v').last()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("{}/{}-v{}", pr, reviewer, last_version + 1)
    };

    if client.create_ref(&next_ref, sha).await.is_err() {
        return Err(());
    }

    Ok(())
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
        let num = 1234;

        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v1")), eq(sha))
            .returning(|_, _| Ok(()));
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/head")), eq(sha))
            .returning(|_, _| Ok(()));
        let r = open_pr(mock, num, sha).await;
        assert!(r.is_ok())
    }

    #[tokio::test]
    async fn test_close_pr() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let refs = vec![
            format!("{num}/v1"),
            format!("{num}/v2"),
            format!("{num}/head"),
            format!("{num}/reviewer-v1"),
            format!("{num}/reviewer-v2"),
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

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/")))
            .returning(move |_| {
                let refs = vec![
                    format!("refs/chetter/{num}/head"),
                    format!("refs/chetter/{num}/v4"),
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
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/v5")), eq(sha))
            .returning(|_, _| Ok(()));
        let r = synchronize_pr(mock, num, sha).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_synchronize_pr_no_head() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let sha = "abc123";

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/")))
            .returning(move |_| {
                let refs = vec![
                    format!("refs/chetter/{num}/v4"),
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
            .with(eq(format!("{num}/v5")), eq(sha))
            .returning(|_, _| Ok(()));
        let r = synchronize_pr(mock, num, sha).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_bookmark_pr() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let sha = "abc123";
        let user = "me";

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/{user}")))
            .returning(move |_| {
                let refs = vec![
                    format!("refs/chetter/{num}/{user}-head"),
                    format!("refs/chetter/{num}/{user}-v1"),
                    format!("refs/chetter/{num}/{user}-v2"),
                    format!("refs/chetter/{num}/{user}-v3"),
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
        mock.expect_create_ref()
            .times(1)
            .with(eq(format!("{num}/{user}-v4")), eq(sha))
            .returning(|_, _| Ok(()));
        let r = bookmark_pr(mock, num, user, sha).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_bookmark_pr_no_head() {
        let mut mock = MockRepositoryController::new();
        let num = 1234;
        let sha = "abc123";
        let user = "me";

        mock.expect_matching_refs()
            .times(1)
            .with(eq(format!("{num}/{user}")))
            .returning(move |_| {
                let refs = vec![
                    format!("refs/chetter/{num}/{user}-v3"),
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
            .with(eq(format!("{num}/{user}-v4")), eq(sha))
            .returning(|_, _| Ok(()));
        let r = bookmark_pr(mock, num, user, sha).await;
        assert!(r.is_ok());
    }
}
