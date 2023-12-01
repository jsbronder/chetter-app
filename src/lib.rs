use octocrab::{models::repos::Ref, params::repos::Reference, Octocrab};
use serde_json::json;
use tracing::{error, info};

pub async fn open_pr(
    client: &Octocrab,
    org: &str,
    repo: &str,
    pr: u64,
    sha: &str,
) -> Result<(), ()> {
    let mut failed = false;
    for ref_name in &["head", "v1"] {
        if create_ref(client, org, repo, &format!("{}/{}", pr, ref_name), sha)
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
    client: &Octocrab,
    org: &str,
    repo: &str,
    pr: u64,
    sha: &str,
) -> Result<(), ()> {
    let refs: Vec<Ref> = match client
        .get(
            format!("/repos/{}/{}/git/matching-refs/chetter/{}/", org, repo, pr),
            None::<&()>,
        )
        .await
    {
        Ok(v) => v,
        Err(octocrab::Error::GitHub { source, .. }) => {
            error!("github: {}", source.message);
            return Err(());
        }
        Err(error) => {
            error!("failed to get pr refs: {:?}", error);
            return Err(());
        }
    };

    if refs.iter().any(|t| t.ref_field.ends_with("/head")) {
        let ref_name = format!("{}/head", pr);
        let _ = update_ref(client, org, repo, &ref_name, sha).await;
    } else {
        let ref_name = format!("{}/head", pr);
        let _ = create_ref(client, org, repo, &ref_name, sha).await;
    }

    let next_ref = if refs.is_empty() {
        format!("{}/v1", pr)
    } else {
        let last_version: u32 = refs
            .iter()
            .filter_map(|t| t.ref_field.split('v').last()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("{}/v{}", pr, last_version + 1)
    };

    if create_ref(client, org, repo, &next_ref, sha).await.is_err() {
        return Err(());
    }

    Ok(())
}

async fn update_ref(
    client: &Octocrab,
    org: &str,
    repo: &str,
    ref_name: &str,
    sha: &str,
) -> Result<(), ()> {
    let req = json!({"sha": &sha, "force": true});
    let url = format!("/repos/{}/{}/git/refs/chetter/{}", org, repo, ref_name);
    let rep: Result<Ref, _> = client.post(&url, Some(&req)).await;
    if let Err(error) = rep {
        match error {
            octocrab::Error::GitHub { source, .. } => {
                error!(
                    "Failed to update {} to {}: {}",
                    ref_name,
                    &sha[0..8],
                    source.message
                );
            }
            error => {
                error!(
                    "Failed to update {} to {}: {:?}",
                    ref_name,
                    &sha[0..8],
                    error
                );
            }
        }
        Err(())
    } else {
        info!("updated refs/chetter/{} as {}", ref_name, &sha[0..8]);
        Ok(())
    }
}

async fn create_ref(
    client: &Octocrab,
    org: &str,
    repo: &str,
    ref_name: &str,
    sha: &str,
) -> Result<(), ()> {
    // We use Commit so that we can use a full refspec, refs/chetter/..., that won't get modified
    // by ref_url() or full_ref_url().
    let full_ref = Reference::Commit(format!("refs/chetter/{}", ref_name));
    if let Err(error) = client.repos(org, repo).create_ref(&full_ref, sha).await {
        match error {
            octocrab::Error::GitHub { source, .. } => {
                error!(
                    "Failed to create {} as {}: {}",
                    ref_name,
                    &sha[0..8],
                    &source.message
                );
            }
            error => {
                error!(
                    "Failed to create {} as {}: {:?}",
                    ref_name,
                    &sha[0..8],
                    &error
                );
            }
        }
        Err(())
    } else {
        info!("created refs/chetter/{} as {}", ref_name, &sha[0..8]);
        Ok(())
    }
}
