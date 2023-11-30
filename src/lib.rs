use octocrab::{models::repos::Ref, params::repos::Reference, Octocrab};
use tracing::{error, info};

pub async fn synchronize_pr(
    client: &Octocrab,
    org: &str,
    repo: &str,
    pr: u64,
    sha: &str,
) -> Result<(), ()> {
    let refs: Vec<Ref> = match client
        .get(
            format!("/repos/{}/{}/git/matching-refs/chetter/{}/v", org, repo, pr),
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

    // We use Commit so that we can use a full refspec, refs/chetter/..., that won't get modified
    // by ref_url() or full_ref_url().
    let next_ref: Reference = if refs.is_empty() {
        Reference::Commit(format!("refs/chetter/{}/v1", pr))
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
        Reference::Commit(format!("refs/chetter/{}/v{}", pr, last_version + 1))
    };

    if let Err(error) = client.repos(org, repo).create_ref(&next_ref, sha).await {
        error!("Failed to make tag: {:?}", error);
        return Err(());
    }
    info!("Created {}", &next_ref);

    Ok(())
}
