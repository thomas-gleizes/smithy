//! Dépôts ouverts récemment, par serveur : l'onglet « Récents » de l'accueil. Aucune API ne
//! donne ça, c'est un historique local (références seulement, jamais de contenu).

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::session::{create_private_dir, write_private};

/// Au-delà, les plus anciens sont oubliés.
pub const MAX_RECENT: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub server: String,
    pub owner: String,
    pub name: String,
    /// Secondes depuis 1970.
    pub opened: i64,
}

/// Met `entry` en tête, sans doublon du même dépôt sur le même serveur, et borne la liste.
pub fn push(entries: &mut Vec<RecentEntry>, entry: RecentEntry) {
    entries
        .retain(|e| !(e.server == entry.server && e.owner == entry.owner && e.name == entry.name));
    entries.insert(0, entry);
    entries.truncate(MAX_RECENT);
}

/// Réunit l'historique du disque et celui de cette instance : deux instances en parallèle ne
/// s'effacent pas mutuellement. Pour un même dépôt on garde la date la plus récente.
pub fn merge(disk: Vec<RecentEntry>, mine: &[RecentEntry]) -> Vec<RecentEntry> {
    let mut all: Vec<RecentEntry> = mine.to_vec();
    for d in disk {
        match all
            .iter_mut()
            .find(|e| e.server == d.server && e.owner == d.owner && e.name == d.name)
        {
            Some(e) => e.opened = e.opened.max(d.opened),
            None => all.push(d),
        }
    }
    all.sort_by_key(|e| std::cmp::Reverse(e.opened));
    all.truncate(MAX_RECENT);
    all
}

pub struct RecentStore {
    path: PathBuf,
}

impl RecentStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// `~/.local/state/smithy/recent.json` (ou l'équivalent de la plateforme).
    pub fn default_location() -> Option<Self> {
        let d = directories::ProjectDirs::from("", "", "smithy")?;
        let base = d.state_dir().unwrap_or_else(|| d.data_local_dir());
        Some(Self::new(base.join("recent.json")))
    }

    /// Vide si absent ou illisible : un historique perdu ne vaut pas une erreur au démarrage.
    pub fn load(&self) -> Vec<RecentEntry> {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Sauvegarde en fusionnant avec ce que le disque contient maintenant, et renvoie le résultat.
    pub fn save_merged(&self, mine: &[RecentEntry]) -> Result<Vec<RecentEntry>> {
        let merged = merge(self.load(), mine);
        self.save(&merged)?;
        Ok(merged)
    }

    pub fn save(&self, entries: &[RecentEntry]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            create_private_dir(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(entries)?;
        write_private(&tmp, &json).with_context(|| format!("écriture de {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("remplacement de {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(server: &str, name: &str, opened: i64) -> RecentEntry {
        RecentEntry {
            server: server.into(),
            owner: "o".into(),
            name: name.into(),
            opened,
        }
    }

    #[test]
    fn fusion_de_deux_instances() {
        let disk = vec![entry("s", "a", 50), entry("s", "autre", 40)];
        let mine = vec![entry("s", "b", 60), entry("s", "a", 30)];
        let m = merge(disk, &mine);
        let names: Vec<_> = m.iter().map(|e| (e.name.as_str(), e.opened)).collect();
        // « a » garde sa date la plus récente, l'ordre est du plus récent au plus ancien
        assert_eq!(names, [("b", 60), ("a", 50), ("autre", 40)]);
        // Borne respectée
        let many: Vec<_> = (0..40).map(|i| entry("s", &format!("r{i}"), i)).collect();
        assert_eq!(merge(many, &[]).len(), MAX_RECENT);
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("smithy-recent-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn le_plus_recent_en_tete_sans_doublon() {
        let mut v = Vec::new();
        push(&mut v, entry("s", "a", 1));
        push(&mut v, entry("s", "b", 2));
        push(&mut v, entry("s", "a", 3)); // réouvert : remonte, pas de doublon
        assert_eq!(
            v.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(v[0].opened, 3);
        // Le même nom sur un autre serveur est un autre dépôt
        push(&mut v, entry("autre", "a", 4));
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn liste_bornee() {
        let mut v = Vec::new();
        for i in 0..(MAX_RECENT + 10) {
            push(&mut v, entry("s", &format!("r{i}"), i as i64));
        }
        assert_eq!(v.len(), MAX_RECENT);
        assert_eq!(
            v[0].name,
            format!("r{}", MAX_RECENT + 9),
            "les plus récents sont gardés"
        );
    }

    #[test]
    fn aller_retour_et_fichier_illisible() {
        let tmp = Tmp::new("roundtrip");
        let store = RecentStore::new(tmp.0.join("etat").join("recent.json"));
        assert!(store.load().is_empty(), "absent : vide");
        let v = vec![entry("s", "a", 1), entry("s", "b", 2)];
        store.save(&v).unwrap();
        assert_eq!(store.load(), v);
        fs::write(&store.path, "pas du json").unwrap();
        assert!(store.load().is_empty(), "illisible : vide, pas d'erreur");
    }

    #[cfg(unix)]
    #[test]
    fn fichier_prive() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = Tmp::new("perms");
        let store = RecentStore::new(tmp.0.join("recent.json"));
        store.save(&[entry("s", "a", 1)]).unwrap();
        let mode = fs::metadata(&store.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!store.path.with_extension("json.tmp").exists());
    }
}
