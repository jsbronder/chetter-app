use async_trait::async_trait;
use octocrab::{
    models::{
        webhook_events::{EventInstallation, WebhookEvent},
        InstallationToken,
    },
    params::repos::Reference,
    Octocrab,
};
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info, warn};

#[cfg(test)]
use mockall::automock;

use crate::error::ChetterError;

#[derive(Debug, Clone)]
pub struct Ref {
    pub full_name: String,
    pub sha: String,
}

#[derive(Debug, Clone)]
pub struct AppClient {
    crab: Octocrab,
}

impl AppClient {
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

    pub async fn repo_client(self, ev: &WebhookEvent) -> Result<RepositoryClient, ChetterError> {
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

pub struct RepositoryClient {
    crab: Octocrab,
    org: String,
    repo: String,
}

impl RepositoryClient {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }
}

#[cfg_attr(test, automock)]
#[async_trait]
pub trait RepositoryController {
    async fn create_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError>;
    async fn update_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError>;
    async fn delete_ref(&self, ref_name: &str) -> Result<(), ChetterError>;
    async fn matching_refs(&self, search: &str) -> Result<Vec<Ref>, ChetterError>;
}

#[async_trait]
impl RepositoryController for RepositoryClient {
    async fn create_ref(&self, ref_name: &str, sha: &str) -> Result<(), ChetterError> {
        // We use Commit so that we can use a full refspec, refs/chetter/..., that won't get
        // modified by ref_url() or full_ref_url().
        let full_ref = Reference::Commit(format!("refs/chetter/{}", ref_name));
        match self
            .crab
            .repos(&self.org, &self.repo)
            .create_ref(&full_ref, sha)
            .await
        {
            Ok(_) => {
                info!("created refs/chetter/{} as {}", ref_name, &sha[0..8]);
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
            "/repos/{}/{}/git/refs/chetter/{}",
            self.org, self.repo, ref_name
        );
        match self.crab.post(&url, Some(&req)).await {
            Ok::<octocrab::models::repos::Ref, _>(_) => {
                info!("updated refs/chetter/{} as {}", ref_name, &sha[0..8]);
                Ok(())
            }
            Err(error) => {
                error!("Failed to update {} to {}", ref_name, &sha[0..8]);
                Err(ChetterError::Octocrab(error))
            }
        }
    }

    async fn delete_ref(&self, ref_name: &str) -> Result<(), ChetterError> {
        match self
            .crab
            ._delete(
                format!(
                    "/repos/{}/{}/git/refs/chetter/{}",
                    self.org, self.repo, ref_name
                ),
                None::<&()>,
            )
            .await
        {
            Ok(_) => {
                info!("deleted chetter/{}", ref_name);
                Ok(())
            }
            Err(error) => {
                error!("failed to delete chetter/{}: {:?}", ref_name, &error);
                Err(ChetterError::Octocrab(error))
            }
        }
    }

    async fn matching_refs(&self, search: &str) -> Result<Vec<Ref>, ChetterError> {
        match self
            .crab
            .get(
                format!(
                    "/repos/{}/{}/git/matching-refs/chetter/{}",
                    self.org, self.repo, search
                ),
                None::<&()>,
            )
            .await
        {
            Ok::<Vec<octocrab::models::repos::Ref>, _>(v) => Ok(v
                .iter()
                .filter_map(|r| {
                    let sha = match &r.object {
                        octocrab::models::repos::Object::Commit { sha, .. } => sha,
                        octocrab::models::repos::Object::Tag { sha, .. } => sha,
                        _ => {
                            warn!("Skipping unmatched: {:?}", r);
                            return None;
                        }
                    };

                    Some(Ref {
                        full_name: r.ref_field.clone(),
                        sha: sha.clone(),
                    })
                })
                .collect()),
            Err(error) => {
                error!("failed to match chetter/{}: {}", search, error);
                Err(ChetterError::Octocrab(error))
            }
        }
    }
}
