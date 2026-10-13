use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug)]
pub struct Config {
    pub url: String,
    pub token: String,
}

#[derive(Deserialize, Default)]
struct FileConfig {
    url: Option<String>,
    token: Option<String>,
}

pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "smithy").map(|d| d.config_dir().join("config.toml"))
}

/// Les variables `SMITHY_URL` / `SMITHY_TOKEN` priment sur le fichier de config.
pub fn load() -> Result<Config> {
    let file = match config_path() {
        Some(p) if p.exists() => Some(
            std::fs::read_to_string(&p).with_context(|| format!("lecture de {}", p.display()))?,
        ),
        _ => None,
    };
    resolve(file.as_deref(), |k| std::env::var(k).ok())
}

fn resolve(file: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    let file: FileConfig = match file {
        Some(s) => toml::from_str(s).context("config.toml invalide")?,
        None => FileConfig::default(),
    };
    let url = env("SMITHY_URL").or(file.url).filter(|s| !s.is_empty());
    let token = env("SMITHY_TOKEN").or(file.token).filter(|s| !s.is_empty());
    match (url, token) {
        (Some(url), Some(token)) => Ok(Config { url, token }),
        _ => {
            let hint = config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            bail!(
                "configuration incomplète : définir SMITHY_URL et SMITHY_TOKEN, \
                 ou `url` et `token` dans {hint}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn lit_le_fichier() {
        let c = resolve(Some("url = \"http://a\"\ntoken = \"t\""), no_env).unwrap();
        assert_eq!((c.url.as_str(), c.token.as_str()), ("http://a", "t"));
    }

    #[test]
    fn l_env_prime_sur_le_fichier() {
        let env = |k: &str| (k == "SMITHY_URL").then(|| "http://env".to_owned());
        let c = resolve(Some("url = \"http://a\"\ntoken = \"t\""), env).unwrap();
        assert_eq!(c.url, "http://env");
        assert_eq!(c.token, "t");
    }

    #[test]
    fn config_incomplete() {
        assert!(resolve(None, no_env).is_err());
        assert!(resolve(Some("url = \"http://a\""), no_env).is_err());
    }
}
