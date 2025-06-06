use async_trait::async_trait;
use indoc::formatdoc;
use octocrab::{
    models::{
        webhook_events::{EventInstallation, WebhookEvent},
        InstallationToken,
    },
    Octocrab,
};
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info, warn};

#[cfg(test)]
use mockall::automock;

use crate::error::{ChetterError, GraphqlErrors};

/// Namespace under which all references will be created.
// This has to be under refs/heads, refs/tags, refs/notes or refs/guest in order to use GraphQL per
// https://github.com/orgs/community/discussions/83980.  GraphQL is important so that we can delete
// hundreds of references with a single API call when a PR is closed.
const REF_NS: &str = "refs/heads/pr";

/// Git reference
#[derive(Debug, Clone, PartialEq)]
pub struct Ref {
    /// Symbolic reference name
    pub full_name: String,

    /// Full SHA-1 object name
    pub sha: String,

    /// GraphQL node_id
    pub node_id: String,
}

/// GitHub Application Client.
///
/// A GitHub client authenticated as a 'Github App' as opposed to an 'OAuth 2' application.  This
/// client is mostly useful for creating a `RepositoryClient`, which can get an installation access
/// token and then take actions on GitHub repositories where it has been installed.
#[derive(Debug, Clone)]
pub struct AppClient {
    crab: Octocrab,
}

impl AppClient {
    /// Create a new AppClient from a configuration file.
    pub fn new(config_path: String) -> Result<Self, ChetterError> {
        #[derive(Deserialize, Debug)]
        struct Config {
            app_id: u64,
            private_key: String,
        }

        let config_str = std::fs::read_to_string(config_path)?;
        let config: Config = toml::from_str(&config_str)?;
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(config.private_key.as_bytes())?;

        let crab = Octocrab::builder().app(config.app_id.into(), key).build()?;

        Ok(Self { crab })
    }

    /// Create a new RepositoryClient using the `.installation` data in a webhook event.
    pub async fn repo_client(&self, ev: &WebhookEvent) -> Result<RepositoryClient, ChetterError> {
        let repo = ev
            .repository
            .as_ref()
            .ok_or(ChetterError::GithubParseError("missing .repository".into()))?;

        let org = repo
            .owner
            .as_ref()
            .ok_or(ChetterError::GithubParseError(
                "missing .repository.owner".into(),
            ))?
            .login
            .clone();

        let id = match ev.installation.as_ref() {
            Some(EventInstallation::Minimal(v)) => v.id.0,
            Some(EventInstallation::Full(v)) => v.id.0,
            None => {
                return Err(ChetterError::GithubParseError(
                    "missing event.installation.id".into(),
                ));
            }
        };
        let url = format!("/app/installations/{}/access_tokens", id);
        let token: InstallationToken = self.crab.post(url, None::<&()>).await?;
        let crab = octocrab::OctocrabBuilder::new()
            .personal_token(token.token)
            .build()?;

        Ok(RepositoryClient {
            crab,
            org,
            repo: repo.name.clone(),
        })
    }
}

/// GitHub client authorized to act on behalf of a 'GitHub App' using the granted permissions on a
/// specific repository.
pub struct RepositoryClient {
    crab: Octocrab,
    org: String,
    repo: String,
}

impl RepositoryClient {
    /// Get the full name for the target repository.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }
}

#[cfg_attr(test, automock)]
#[async_trait]
/// Types that can control symbolic git references in a repository.
///
/// The API ensures that all references are located under {REF_NS}.
///
/// # Examples
///
/// ```
/// use async_trait::async_trait;
/// use chetter_app::{
///     error::ChetterError,
///     github::{Ref, RepositoryController}
/// };
///
/// struct NullClient;
///
/// #[async_trait]
/// impl RepositoryController for NullClient {
///     async fn create_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError> { Ok(()) }
///     async fn update_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError> { Ok(()) }
///     async fn delete_refs(&self, ref_names: &[Ref]) -> Result<(), ChetterError> { Ok(()) }
///     async fn matching_refs(&self, search: &str) -> Result<Vec<Ref>, ChetterError> { Ok(vec![]) }
/// }
///
/// async fn foo() {
///     let client = NullClient;
///
///     // Update `{REF_NS}/1234/existing-ref` to sha `abc1234`
///     assert!(client.create_ref("1234/existing-ref", "abc1234").await.is_ok());
/// }
/// ```
pub trait RepositoryController {
    /// Create a new reference (rooted at {REF_NS}/*) to the specified sha.
    async fn create_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError>;

    /// Update an existing reference (rooted at *{REF_NS}/*) to the specified sha.
    async fn update_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError>;

    /// Delete existing references (rooted at *{REF_NS}/*).
    async fn delete_refs(&self, ref_names: &[Ref]) -> Result<(), ChetterError>;

    /// Get a vector of references (rooted at *{REF_NS}/*) that end with the specified search
    /// string.
    ///
    /// For example `controller.matching_refs("abc/d")` will match:
    ///     - {REF_NS}/abc/def
    ///     - {REF_NS}/abc/d/ef
    ///     - {REF_NS}/abc/d
    /// but will not match:
    ///     - {REF_NS}/other/abc/d
    ///     - {REF_NS}/ab
    async fn matching_refs(&self, search: &str) -> Result<Vec<Ref>, ChetterError>;
}

#[async_trait]
impl RepositoryController for RepositoryClient {
    async fn create_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError> {
        // We can't use self.crab.repos.create_ref as we want to be able to support references that
        // are not a tag or a branch.  However, the Reference::{Tag,Branch} passed into create_ref
        // will append /refs/heads or /refs/tags when create_ref calls full_ref_url on it.
        let page = format!("/repos/{}/{}/git/refs", self.org, self.repo);
        let req = json!({
            "ref": format!("{}/{}", REF_NS, ref_name),
            "sha": sha});

        match self.crab.post(page, Some(&req)).await {
            Ok::<octocrab::models::repos::Ref, _>(_) => {
                info!("created {}/{} as {}", REF_NS, ref_name, &sha[0..8]);
                Ok(())
            }
            Err(error) => {
                error!("Failed to create {} as {}", ref_name, &sha[0..8]);
                Err(ChetterError::Octocrab(error))
            }
        }
    }

    async fn update_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError> {
        let req = json!({"sha": &sha, "force": true});
        let url = format!(
            "/repos/{}/{}/git/{}/{}",
            self.org, self.repo, REF_NS, ref_name
        );
        match self.crab.post(&url, Some(&req)).await {
            Ok::<octocrab::models::repos::Ref, _>(_) => {
                info!("updated {}/{} as {}", REF_NS, ref_name, &sha[0..8]);
                Ok(())
            }
            Err(error) => {
                error!("Failed to update {}/{} to {}", REF_NS, ref_name, &sha[0..8]);
                Err(ChetterError::Octocrab(error))
            }
        }
    }

    async fn delete_refs(&self, refs: &[Ref]) -> Result<(), ChetterError> {
        let mut errors: Vec<ChetterError> = vec![];

        // Github GraphQL takes a ridiculous amount of time to delete references and will cut us
        // off after 90s of CPU time or 60s of real time.
        for chunk in refs.chunks(100) {
            let mutations: String = chunk
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    formatdoc!(
                        r#"
                        delete_{i}: deleteRef(input: {{
                                refId: "{node_id}",
                                clientMutationId: "{full_name}"
                            }}) {{
                            clientMutationId
                        }}
                        "#,
                        node_id = r.node_id,
                        full_name = r.full_name,
                    )
                })
                .collect();
            let query = json!({"query": format!("mutation {{\n{}\n}}", mutations)});
            info!("Sending mutation to delete {} refs", chunk.len());

            match self.crab.graphql(&query).await {
                // graphql errors are ignored
                // https://github.com/XAMPPRocky/octocrab/issues/78
                Ok::<serde_json::Value, _>(resp) => {
                    if let Ok(e) = serde_json::from_value::<GraphqlErrors>(resp) {
                        e.errors.iter().for_each(|e| {
                            error!("error: {}", e.message);
                        });
                        errors.push(ChetterError::GithubGraphqlError(e));
                    } else {
                        chunk.iter().for_each(|r| {
                            info!("deleted {}/{}", REF_NS, r.full_name);
                        });
                    }
                }
                Err(error) => {
                    error!("failed to delete references: {:?}", &error);
                    errors.push(ChetterError::Octocrab(error));
                }
            };
        }

        match errors.pop() {
            None => Ok(()),
            Some(e) => Err(e),
        }
    }

    async fn matching_refs(&self, search: &str) -> Result<Vec<Ref>, ChetterError> {
        let short_ns = &REF_NS[5..]; // Strip 'refs/'
        let page = self
            .crab
            .get(
                format!(
                    "/repos/{}/{}/git/matching-refs/{}/{}",
                    self.org, self.repo, short_ns, search
                ),
                None::<&()>,
            )
            .await?;
        let results = self
            .crab
            .all_pages::<octocrab::models::repos::Ref>(page)
            .await?;
        Ok(results
            .into_iter()
            .filter_map(|r| {
                let sha = match r.object {
                    octocrab::models::repos::Object::Commit { sha, .. } => sha,
                    octocrab::models::repos::Object::Tag { sha, .. } => sha,
                    _ => {
                        warn!("Skipping unmatched: {:?}", r);
                        return None;
                    }
                };

                Some(Ref {
                    full_name: r.ref_field.replace(&format!("{REF_NS}/"), ""),
                    sha,
                    node_id: r.node_id,
                })
            })
            .collect())
    }
}
