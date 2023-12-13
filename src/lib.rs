use error::ChetterError;
use github::RepositoryController;

pub mod error;
pub mod github;

pub async fn open_pr(
    client: impl RepositoryController,
    pr: u64,
    sha: &str,
    base: &str,
) -> Result<(), ()> {
    let mut failed = false;
    for ref_name in ["head", "v1"] {
        for (suffix, target) in [("", sha), ("-base", base)] {
            if client
                .create_ref(&format!("{}/{}{}", pr, ref_name, suffix), target)
                .await
                .is_err()
            {
                failed = true;
            }
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
    base: &str,
) -> Result<(), ()> {
    let Ok(refs) = client.matching_refs(&format!("{}/", pr)).await else {
        return Err(());
    };
    let mut results: Vec<Result<(), ChetterError>> = vec![];

    for (name, target) in [("head", sha), ("head-base", base)] {
        let name = format!("{pr}/{name}");
        if refs.iter().any(|t| t.full_name.ends_with(&name)) {
            results.push(client.update_ref(&name, target).await);
        } else {
            results.push(client.create_ref(&name, target).await);
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
        results.push(client.create_ref(&name, target).await);
    }

    if results.iter().any(|r| r.is_err()) {
        Err(())
    } else {
        Ok(())
    }
}

pub async fn bookmark_pr(
    client: impl RepositoryController,
    pr: u64,
    reviewer: &str,
    sha: &str,
    base: &str,
) -> Result<(), ()> {
    let Ok(refs) = client.matching_refs(&format!("{}/{}", pr, reviewer)).await else {
        return Err(());
    };
    let mut results: Vec<Result<(), ChetterError>> = vec![];

    for (suffix, target) in [("head", sha), ("head-base", base)] {
        let name = format!("{pr}/{reviewer}-{suffix}");
        if refs.iter().any(|t| t.full_name.ends_with(&suffix)) {
            results.push(client.update_ref(&name, target).await);
        } else {
            results.push(client.create_ref(&name, target).await);
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
        results.push(client.create_ref(&name, target).await);
    }

    if results.iter().any(|r| r.is_err()) {
        Err(())
    } else {
        Ok(())
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
