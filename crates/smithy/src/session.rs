//! Cookie de session persistant, un fichier par serveur. C'est un secret (autant
//! que le mot de passe) : dossier en 0700, fichiers en 0600, jamais dans la config.
//! Le mot de passe, lui, n'est jamais écrit.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::valid_name;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// URL du serveur au moment du login : si elle change, la session ne vaut plus.
    pub url: String,
    pub cookies: BTreeMap<String, String>,
}

pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `~/.local/state/smithy/sessions` (ou l'équivalent de la plateforme).
    pub fn default_location() -> Option<Self> {
        let d = directories::ProjectDirs::from("", "", "smithy")?;
        let base = d.state_dir().unwrap_or_else(|| d.data_local_dir());
        Some(Self::new(base.join("sessions")))
    }

    fn file(&self, name: &str) -> Result<PathBuf> {
        if !valid_name(name) {
            bail!("nom de serveur invalide pour une session : « {name} »");
        }
        Ok(self.dir.join(format!("{name}.json")))
    }

    /// `None` si absente, illisible ou obtenue pour une autre URL : il suffit
    /// alors de se reconnecter.
    pub fn load(&self, name: &str, url: &str) -> Result<Option<Session>> {
        let path = self.file(name)?;
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("lecture de {}", path.display())),
        };
        Ok(serde_json::from_str::<Session>(&raw)
            .ok()
            .filter(|s| s.url == url && !s.cookies.is_empty()))
    }

    pub fn save(&self, name: &str, session: &Session) -> Result<()> {
        let path = self.file(name)?;
        create_private_dir(&self.dir)?;
        // Écriture dans un fichier voisin puis renommage : jamais de session à moitié écrite.
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(session)?;
        write_private(&tmp, &json).with_context(|| format!("écriture de {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("remplacement de {}", path.display()))
    }

    /// Oublie la session (expirée ou déconnexion). Sans effet si elle n'existe pas.
    pub fn clear(&self, name: &str) -> Result<()> {
        let path = self.file(name)?;
        match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("suppression de {}", path.display()))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("création de {}", dir.display()))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    fs::File::create(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossier jetable unique par test, supprimé à la fin.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("smithy-test-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session(url: &str) -> Session {
        Session {
            url: url.into(),
            cookies: BTreeMap::from([("session".to_owned(), "abc".to_owned())]),
        }
    }

    #[test]
    fn aller_retour() {
        let tmp = Tmp::new("roundtrip");
        let store = SessionStore::new(tmp.0.join("sessions"));
        assert_eq!(store.load("forge", "http://a").unwrap(), None);
        store.save("forge", &session("http://a")).unwrap();
        assert_eq!(
            store.load("forge", "http://a").unwrap(),
            Some(session("http://a"))
        );
    }

    #[test]
    fn session_d_une_autre_url_ignoree() {
        let tmp = Tmp::new("url");
        let store = SessionStore::new(tmp.0.clone());
        store.save("forge", &session("http://a")).unwrap();
        assert_eq!(store.load("forge", "http://b").unwrap(), None);
    }

    #[test]
    fn fichier_corrompu_ou_sans_cookie_ignore() {
        let tmp = Tmp::new("corrupt");
        let store = SessionStore::new(tmp.0.clone());
        fs::create_dir_all(&tmp.0).unwrap();
        fs::write(tmp.0.join("forge.json"), "pas du json").unwrap();
        assert_eq!(store.load("forge", "http://a").unwrap(), None);
        let vide = Session {
            url: "http://a".into(),
            cookies: BTreeMap::new(),
        };
        store.save("forge", &vide).unwrap();
        assert_eq!(store.load("forge", "http://a").unwrap(), None);
    }

    #[test]
    fn clear_est_idempotent() {
        let tmp = Tmp::new("clear");
        let store = SessionStore::new(tmp.0.clone());
        store.clear("forge").unwrap();
        store.save("forge", &session("http://a")).unwrap();
        store.clear("forge").unwrap();
        assert_eq!(store.load("forge", "http://a").unwrap(), None);
    }

    #[test]
    fn refuse_les_noms_dangereux() {
        let tmp = Tmp::new("names");
        let store = SessionStore::new(tmp.0.clone());
        assert!(store.save("../evil", &session("x")).is_err());
        assert!(store.load("a/b", "x").is_err());
        assert!(!tmp.0.parent().unwrap().join("evil.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn permissions_restreintes() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = Tmp::new("perms");
        let dir = tmp.0.join("sessions");
        let store = SessionStore::new(dir.clone());
        store.save("forge", &session("http://a")).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("forge.json")), 0o600);
        assert!(!dir.join("forge.json.tmp").exists());
    }
}
