use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Type de forge d'un serveur. Un nouveau backend = une nouvelle variante.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    #[default]
    Forgejo,
    /// github.com, ou GitHub Enterprise (`https://<hôte>/api/v3`). Un token est obligatoire :
    /// l'API n'accepte pas de mot de passe.
    #[serde(rename = "github")]
    GitHub,
}

impl ServerKind {
    pub fn label(self) -> &'static str {
        match self {
            ServerKind::Forgejo => "forgejo",
            ServerKind::GitHub => "github",
        }
    }

    /// URL proposée à la création d'un serveur de ce type (et prise si `url` est absente).
    pub fn default_url(self) -> &'static str {
        match self {
            ServerKind::Forgejo => "",
            ServerKind::GitHub => smithy_github::DEFAULT_API,
        }
    }
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
    /// Facultative pour GitHub (l'API publique par défaut).
    #[serde(default)]
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

/// Les serveurs **du fichier seul** (l'ancien format devient « default »), sans les variables
/// d'environnement : c'est ce qu'on réécrit quand on modifie la liste depuis l'application.
fn parse_file(file: Option<&str>) -> Result<Vec<Server>> {
    let file: FileConfig = match file {
        Some(s) => toml::from_str(s).context("config.toml invalide")?,
        None => FileConfig::default(),
    };
    let non_empty = |v: Option<String>| v.filter(|s| !s.is_empty());
    let mut servers: Vec<Server> = file
        .server
        .into_iter()
        .map(|s| Server {
            url: if s.url.trim().is_empty() {
                s.kind.default_url().to_owned()
            } else {
                s.url
            },
            name: s.name,
            kind: s.kind,
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
    Ok(servers)
}

fn resolve(file: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    let mut servers = parse_file(file)?;
    let non_empty = |v: Option<String>| v.filter(|s| !s.is_empty());
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

    // Aucun serveur : ce n'est pas une erreur, l'application demande d'en configurer un.
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

/// Vérifie un serveur saisi dans l'application : `others` sont les noms déjà pris.
pub fn check_server(name: &str, url: &str, others: &[&str]) -> Result<(), String> {
    if !valid_name(name) {
        return Err("nom invalide (lettres, chiffres, - _ . uniquement)".to_owned());
    }
    if others.contains(&name) {
        return Err(format!("le nom « {name} » est déjà pris"));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() < 9 {
        return Err("l'URL doit commencer par http:// ou https://".to_owned());
    }
    Ok(())
}

#[derive(Serialize)]
struct OutFile<'a> {
    server: Vec<OutServer<'a>>,
}

#[derive(Serialize)]
struct OutServer<'a> {
    name: &'a str,
    kind: &'static str,
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<&'a str>,
}

fn render(servers: &[Server]) -> Result<String> {
    let out = OutFile {
        server: servers
            .iter()
            .map(|s| OutServer {
                name: &s.name,
                kind: s.kind.label(),
                url: &s.url,
                token: s.token.as_deref(),
            })
            .collect(),
    };
    Ok(toml::to_string(&out)?)
}

/// Ce que contient le fichier de configuration, sans les variables d'environnement.
pub fn file_servers() -> Result<Vec<Server>> {
    match config_path() {
        Some(p) if p.exists() => {
            let raw = std::fs::read_to_string(&p)
                .with_context(|| format!("lecture de {}", p.display()))?;
            parse_file(Some(&raw))
        }
        _ => Ok(Vec::new()),
    }
}

/// Remplace la liste des serveurs du fichier (format `[[server]]`). Les commentaires du fichier
/// ne sont pas conservés. Droits 0600 : il peut contenir un token.
pub fn save_servers(servers: &[Server]) -> Result<()> {
    let path = config_path().context("dossier de configuration introuvable")?;
    save_to(&path, servers)
}

fn save_to(path: &std::path::Path, servers: &[Server]) -> Result<()> {
    let dir = path.parent().context("chemin de configuration invalide")?;
    std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    let tmp = path.with_extension("toml.tmp");
    crate::session::write_private(&tmp, render(servers)?.as_bytes())
        .with_context(|| format!("écriture de {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("remplacement de {}", path.display()))
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
    fn serveur_github_url_par_defaut_et_reecriture() {
        let toml = "[[server]]\nname = \"gh\"\nkind = \"github\"\ntoken = \"ghp_x\"\n\n[[server]]\nname = \"ghe\"\nkind = \"github\"\nurl = \"https://ghe.example/api/v3\"\n";
        let c = resolve(Some(toml), no_env).unwrap();
        assert_eq!(c.servers[0].kind, ServerKind::GitHub);
        assert_eq!(c.servers[0].url, "https://api.github.com");
        assert_eq!(c.servers[0].token.as_deref(), Some("ghp_x"));
        assert_eq!(c.servers[1].url, "https://ghe.example/api/v3");
        // Réécrit puis relu : identique
        let again = resolve(Some(&render(&c.servers).unwrap()), no_env).unwrap();
        assert_eq!(again.servers, c.servers);
        assert!(render(&c.servers).unwrap().contains("kind = \"github\""));
        // Un Forgejo sans URL reste une erreur
        assert!(resolve(Some("[[server]]\nname = \"f\"\n"), no_env).is_err());
    }

    #[test]
    fn configurations_invalides() {
        assert!(resolve(None, no_env).unwrap().servers.is_empty());
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
    fn ecriture_puis_relecture() {
        let servers = vec![
            Server {
                name: "a".into(),
                kind: ServerKind::Forgejo,
                url: "http://a".into(),
                token: Some("t".into()),
            },
            Server {
                name: "b".into(),
                kind: ServerKind::Forgejo,
                url: "https://b".into(),
                token: None,
            },
        ];
        let text = render(&servers).unwrap();
        assert_eq!(parse_file(Some(&text)).unwrap(), servers);
        let dir = std::env::temp_dir().join(format!("smithy-cfg-{}", std::process::id()));
        let path = dir.join("sub").join("config.toml");
        save_to(&path, &servers).unwrap();
        assert_eq!(
            parse_file(Some(&std::fs::read_to_string(&path).unwrap())).unwrap(),
            servers
        );
        assert!(!path.with_extension("toml.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn le_fichier_seul_ignore_l_environnement() {
        let s = parse_file(Some("url = \"http://a\"")).unwrap();
        assert_eq!(s[0].name, "default");
        assert!(parse_file(None).unwrap().is_empty());
    }

    #[test]
    fn verification_d_un_serveur() {
        assert!(check_server("ok", "https://forge.example", &["autre"]).is_ok());
        assert!(check_server("../x", "http://a.b", &[]).is_err());
        assert!(
            check_server("a", "http://a.b", &["a"])
                .unwrap_err()
                .contains("déjà")
        );
        assert!(check_server("a", "forge.example", &[]).is_err());
        assert!(check_server("a", "http://", &[]).is_err());
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
