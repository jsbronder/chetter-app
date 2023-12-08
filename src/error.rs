#[derive(Debug)]
pub enum ChetterError {
    GithubParseError(String),
    IOError(std::io::Error),
    JSONWebTokenError(jsonwebtoken::errors::Error),
    Octocrab(octocrab::Error),
    TOMLParseError(toml::de::Error),
}

impl From<std::io::Error> for ChetterError {
    fn from(error: std::io::Error) -> Self {
        Self::IOError(error)
    }
}

impl From<jsonwebtoken::errors::Error> for ChetterError {
    fn from(error: jsonwebtoken::errors::Error) -> Self {
        Self::JSONWebTokenError(error)
    }
}

impl From<toml::de::Error> for ChetterError {
    fn from(error: toml::de::Error) -> Self {
        Self::TOMLParseError(error)
    }
}

impl From<octocrab::Error> for ChetterError {
    fn from(error: octocrab::Error) -> Self {
        Self::Octocrab(error)
    }
}

impl std::error::Error for ChetterError {}

impl std::fmt::Display for ChetterError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ChetterError::GithubParseError(e) => write!(f, "{}", e),
            ChetterError::IOError(e) => write!(f, "{}", e),
            ChetterError::JSONWebTokenError(e) => write!(f, "{}", e),
            ChetterError::Octocrab(e) => write!(f, "{}", e),
            ChetterError::TOMLParseError(e) => write!(f, "{}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_error() {
        use std::io::Error;
        let err = ChetterError::IOError(Error::from_raw_os_error(2));
        assert_eq!("No such file or directory (os error 2)", err.to_string());
    }
}
