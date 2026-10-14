//! Les onglets ouverts, sauvegardés pour être rouverts au lancement suivant.
//!
//! Une seule instance de smithy à la fois gère cette sauvegarde : elle tient un verrou de
//! fichier (libéré par le système à la fin du processus, même après un plantage : pas de
//! verrou périmé à nettoyer). Une deuxième instance démarre à vide et ne sauvegarde rien, sinon
//! elle rouvrirait les onglets de la première en double puis les écraserait.
//!
//! On ne garde que des références (serveur, dépôt, dossier, branche), jamais de contenu. Les logs
//! d'un job ne sont pas restaurés (ils demandent le run entier) : l'onglet revient à la liste des runs.

use std::{
    fs::{self, File, TryLockError},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use smithy_core::{PullFilter, Repo};

use crate::session::{create_private_dir, write_private};

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "page", rename_all = "snake_case")]
pub enum SavedPage {
    Home {
        section: usize,
        selected: usize,
    },
    Repo {
        repo: Repo,
        selected: usize,
    },
    Runs {
        repo: Repo,
    },
    Commits {
        repo: Repo,
        reference: String,
    },
    Branches {
        repo: Repo,
    },
    Pulls {
        repo: Repo,
        #[serde(default)]
        filter: PullFilter,
    },
    /// Une PR : seule sa référence est gardée, tout est redemandé.
    Pull {
        repo: Repo,
        number: u64,
    },
    Browser {
        repo: Repo,
        reference: String,
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTab {
    pub server: String,
    pub pages: Vec<SavedPage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTabs {
    #[serde(default)]
    pub version: u32,
    pub active: usize,
    pub tabs: Vec<SavedTab>,
}

impl SavedTabs {
    pub fn new(active: usize, tabs: Vec<SavedTab>) -> Self {
        Self {
            version: VERSION,
            active,
            tabs,
        }
    }
}

/// Qui gère la sauvegarde des onglets dans cette instance ?
pub enum Ownership {
    /// Cette instance : elle restaure et sauvegarde. Le fichier verrouillé reste ouvert.
    Owner(#[allow(dead_code)] File),
    /// Une autre instance tourne déjà : on ne touche à rien.
    Other,
    /// Verrou impossible (système de fichiers sans verrou) : pas de restauration, sans bruit.
    Unavailable,
}

pub fn acquire(state_dir: &Path) -> Ownership {
    if create_private_dir(state_dir).is_err() {
        return Ownership::Unavailable;
    }
    let file = match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir.join("instance.lock"))
    {
        Ok(f) => f,
        Err(_) => return Ownership::Unavailable,
    };
    match file.try_lock() {
        Ok(()) => Ownership::Owner(file),
        Err(TryLockError::WouldBlock) => Ownership::Other,
        Err(TryLockError::Error(_)) => Ownership::Unavailable,
    }
}

/// `~/.local/state/smithy` (ou l'équivalent de la plateforme).
pub fn state_dir() -> Option<PathBuf> {
    let d = directories::ProjectDirs::from("", "", "smithy")?;
    Some(
        d.state_dir()
            .unwrap_or_else(|| d.data_local_dir())
            .to_path_buf(),
    )
}

pub struct TabsStore {
    path: PathBuf,
}

impl TabsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn in_dir(dir: &Path) -> Self {
        Self::new(dir.join("tabs.json"))
    }

    /// `None` si absent, illisible ou d'une version inconnue : on repart d'un accueil.
    pub fn load(&self) -> Option<SavedTabs> {
        let saved: SavedTabs = serde_json::from_str(&fs::read_to_string(&self.path).ok()?).ok()?;
        (saved.version == VERSION && !saved.tabs.is_empty()).then_some(saved)
    }

    pub fn save(&self, tabs: &SavedTabs) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            create_private_dir(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, &serde_json::to_vec_pretty(tabs)?)
            .with_context(|| format!("écriture de {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("remplacement de {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("smithy-ws-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn repo() -> Repo {
        Repo {
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn sample() -> SavedTabs {
        SavedTabs::new(
            1,
            vec![
                SavedTab {
                    server: "a".into(),
                    pages: vec![SavedPage::Home {
                        section: 2,
                        selected: 3,
                    }],
                },
                SavedTab {
                    server: "b".into(),
                    pages: vec![
                        SavedPage::Home {
                            section: 0,
                            selected: 0,
                        },
                        SavedPage::Repo {
                            repo: repo(),
                            selected: 1,
                        },
                        SavedPage::Browser {
                            repo: repo(),
                            reference: "feature/x".into(),
                            path: "src/a b".into(),
                        },
                    ],
                },
            ],
        )
    }

    #[test]
    fn aller_retour_et_fichier_prive() {
        let tmp = Tmp::new("roundtrip");
        let store = TabsStore::in_dir(&tmp.0);
        assert_eq!(store.load(), None);
        store.save(&sample()).unwrap();
        assert_eq!(store.load(), Some(sample()));
        assert!(!tmp.0.join("tabs.json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(tmp.0.join("tabs.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn fichier_corrompu_ou_d_une_autre_version_ignore() {
        let tmp = Tmp::new("corrupt");
        let store = TabsStore::in_dir(&tmp.0);
        fs::create_dir_all(&tmp.0).unwrap();
        fs::write(tmp.0.join("tabs.json"), "pas du json").unwrap();
        assert_eq!(store.load(), None);
        let mut other = sample();
        other.version = 99;
        store.save(&other).unwrap();
        assert_eq!(store.load(), None);
        store.save(&SavedTabs::new(0, vec![])).unwrap();
        assert_eq!(store.load(), None, "aucun onglet : rien à restaurer");
    }

    #[test]
    fn une_seule_instance_gere_la_sauvegarde() {
        let tmp = Tmp::new("lock");
        let first = acquire(&tmp.0);
        assert!(matches!(first, Ownership::Owner(_)));
        assert!(matches!(acquire(&tmp.0), Ownership::Other));
        // Quand la première s'arrête (verrou libéré), la suivante prend la main
        drop(first);
        assert!(matches!(acquire(&tmp.0), Ownership::Owner(_)));
    }
}
