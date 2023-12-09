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
