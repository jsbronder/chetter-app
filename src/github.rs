use octocrab::Octocrab;
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
}
