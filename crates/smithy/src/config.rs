use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Type de forge d'un serveur. Un nouveau backend = une nouvelle variante.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    #[default]
    Forgejo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub kind: ServerKind,
    pub url: String,
    /// Absent tant que l'utilisateur ne s'est pas connecté (voir la session).
    pub token: Option<String>,
}

#[derive(Debug)]
pub struct Config {
    pub servers: Vec<Server>,
}

impl Config {
    /// Serveur demandé par son nom, ou le premier de la liste.
    pub fn server(&self, wanted: Option<&str>) -> Result<&Server> {
        match wanted {
            None => self.servers.first().context("aucun serveur configuré"),
            Some(n) => self.servers.iter().find(|s| s.name == n).with_context(|| {
                let known: Vec<_> = self.servers.iter().map(|s| s.name.as_str()).collect();
                format!("serveur « {n} » inconnu (connus : {})", known.join(", "))
            }),
        }
    }
}

#[derive(Deserialize, Default)]
struct FileConfig {
    // Ancien format : un seul serveur à la racine.
    url: Option<String>,
    token: Option<String>,
    #[serde(default)]
    server: Vec<FileServer>,
}

#[derive(Deserialize)]
struct FileServer {
    name: String,
    #[serde(default)]
    kind: ServerKind,
    url: String,
    token: Option<String>,
}

const LEGACY_NAME: &str = "default";

pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "smithy").map(|d| d.config_dir().join("config.toml"))
}

/// `SMITHY_URL` / `SMITHY_TOKEN` priment sur le premier serveur du fichier (ou
/// en définissent un si le fichier n'en a pas). `SMITHY_SERVER` choisit le serveur.
pub fn load() -> Result<Config> {
    let file = match config_path() {
        Some(p) if p.exists() => Some(
            std::fs::read_to_string(&p).with_context(|| format!("lecture de {}", p.display()))?,
        ),
        _ => None,
    };
    resolve(file.as_deref(), |k| std::env::var(k).ok())
}

/// Un nom de serveur sert aussi de nom de fichier de session : charset restreint.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}

fn resolve(file: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    let file: FileConfig = match file {
        Some(s) => toml::from_str(s).context("config.toml invalide")?,
        None => FileConfig::default(),
    };
    let non_empty = |v: Option<String>| v.filter(|s| !s.is_empty());

    let mut servers: Vec<Server> = file
        .server
        .into_iter()
        .map(|s| Server {
            name: s.name,
            kind: s.kind,
            url: s.url,
            token: non_empty(s.token),
        })
        .collect();
    if let Some(url) = non_empty(file.url) {
        servers.insert(
            0,
            Server {
                name: LEGACY_NAME.to_owned(),
                kind: ServerKind::Forgejo,
                url,
                token: non_empty(file.token),
            },
        );
    }

    let (env_url, env_token) = (non_empty(env("SMITHY_URL")), non_empty(env("SMITHY_TOKEN")));
    match servers.first_mut() {
        Some(first) => {
            if let Some(u) = env_url {
                first.url = u;
            }
            if let Some(t) = env_token {
                first.token = Some(t);
            }
        }
        None => {
            if let Some(url) = env_url {
                servers.push(Server {
                    name: LEGACY_NAME.to_owned(),
                    kind: ServerKind::Forgejo,
                    url,
                    token: env_token,
                });
            }
        }
    }

    if servers.is_empty() {
        let hint = config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        bail!(
            "aucun serveur configuré : définir SMITHY_URL (et SMITHY_TOKEN), \
             ou déclarer des `[[server]]` dans {hint}"
        )
    }
    let mut seen = HashSet::new();
    for s in &servers {
        if !valid_name(&s.name) {
            bail!(
                "nom de serveur invalide : « {} » (lettres, chiffres, - _ . uniquement)",
                s.name
            );
        }
        if s.url.is_empty() {
            bail!("serveur « {} » : `url` vide", s.name);
        }
        if !seen.insert(s.name.as_str()) {
            bail!("nom de serveur en double : « {} »", s.name);
        }
    }
    Ok(Config { servers })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn ancien_format_devient_le_serveur_default() {
        let c = resolve(Some("url = \"http://a\"\ntoken = \"t\""), no_env).unwrap();
        assert_eq!(c.servers.len(), 1);
        let s = &c.servers[0];
        assert_eq!((s.name.as_str(), s.url.as_str()), ("default", "http://a"));
        assert_eq!(s.token.as_deref(), Some("t"));
        assert_eq!(s.kind, ServerKind::Forgejo);
    }

    #[test]
    fn liste_de_serveurs() {
        let toml = r#"
            [[server]]
            name = "maison"
            url = "http://a"
            token = "t1"

            [[server]]
            name = "travail"
            kind = "forgejo"
            url = "http://b"
        "#;
        let c = resolve(Some(toml), no_env).unwrap();
        assert_eq!(c.servers.len(), 2);
        assert_eq!(c.servers[1].token, None);
        assert_eq!(c.server(Some("travail")).unwrap().url, "http://b");
        assert_eq!(c.server(None).unwrap().name, "maison");
        let err = c.server(Some("nope")).unwrap_err().to_string();
        assert!(err.contains("maison") && err.contains("travail"), "{err}");
    }

    #[test]
    fn l_env_prime_sur_le_premier_serveur() {
        let env = |k: &str| (k == "SMITHY_URL").then(|| "http://env".to_owned());
        let c = resolve(Some("url = \"http://a\"\ntoken = \"t\""), env).unwrap();
        assert_eq!(c.servers[0].url, "http://env");
        assert_eq!(c.servers[0].token.as_deref(), Some("t"));
    }

    #[test]
    fn l_env_seul_suffit() {
        let env = |k: &str| match k {
            "SMITHY_URL" => Some("http://env".to_owned()),
            "SMITHY_TOKEN" => Some("tok".to_owned()),
            _ => None,
        };
        let c = resolve(None, env).unwrap();
        assert_eq!(c.servers[0].name, "default");
        assert_eq!(c.servers[0].token.as_deref(), Some("tok"));
    }

    #[test]
    fn configurations_invalides() {
        assert!(resolve(None, no_env).is_err());
        let dup = "[[server]]\nname=\"a\"\nurl=\"x\"\n[[server]]\nname=\"a\"\nurl=\"y\"";
        assert!(
            resolve(Some(dup), no_env)
                .unwrap_err()
                .to_string()
                .contains("double")
        );
        let traversal = "[[server]]\nname=\"../etc\"\nurl=\"x\"";
        assert!(resolve(Some(traversal), no_env).is_err());
        let kind = "[[server]]\nname=\"a\"\nkind=\"svn\"\nurl=\"x\"";
        assert!(resolve(Some(kind), no_env).is_err());
        let empty_url = "[[server]]\nname=\"a\"\nurl=\"\"";
        assert!(resolve(Some(empty_url), no_env).is_err());
    }

    #[test]
    fn noms_de_serveur() {
        for ok in ["a", "forge-1", "mon_serveur", "v1.2"] {
            assert!(valid_name(ok), "{ok}");
        }
        for ko in ["", ".caché", "a/b", "a b", "é", "a\\b"] {
            assert!(!valid_name(ko), "{ko}");
        }
    }
}
