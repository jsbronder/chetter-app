use octocrab::{
    models::{
        webhook_events::{EventInstallation, WebhookEvent},
        InstallationToken,
    },
    Octocrab,
};
use serde::Deserialize;

use crate::error::ChetterError;

#[derive(Debug, Clone)]
pub struct AppClient {
    pub crab: Octocrab,
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
    pub crab: Octocrab,
    pub org: String,
    pub repo: String,
}
