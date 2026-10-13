//! Version du serveur → adaptateur à utiliser et niveau de support.
//!
//! Les routes web qui servent les logs changent d'une version de Forgejo à l'autre
//! (nom du cookie de session, jeton CSRF, segment `attempt`…). Plutôt qu'un code unique
//! qui suppose la dernière version, chaque famille de versions a un adaptateur
//! ([`Flavor`]) et une entrée dans la table ci-dessous. Seules les versions réellement
//! sondées (`dev/dev.sh probe`, voir `dev/probe-logs.py`) sont déclarées complètes ; les
//! autres sont partielles (on essaie, en le disant) ou non supportées.

/// Famille de comportements des routes web.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// Forgejo 10 à 12 : jeton CSRF exigé (cookie `_csrf` + en-tête), pas de segment
    /// `attempt`, cookies `i_like_gitea` / `gitea_incredible`.
    Legacy,
    /// Forgejo 15 : pas de CSRF, segment `attempt/{n}` obligatoire, cookie `session`.
    Modern,
}

impl Flavor {
    pub(crate) fn needs_csrf(self) -> bool {
        self == Flavor::Legacy
    }

    pub(crate) fn uses_attempts(self) -> bool {
        self == Flavor::Modern
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Full,
    /// Utilisable mais non vérifié : on essaie et on prévient.
    Partial(String),
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// `+gitea-x.y.z` présent : c'est bien un Forgejo (un Gitea répond `1.22.0` tout court).
    pub forgejo: bool,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `"10.0.3+gitea-1.22.0"` → 10.0.3. `None` si illisible.
pub fn parse(raw: &str) -> Option<Version> {
    let raw = raw.trim().trim_start_matches('v');
    let (core, meta) = raw.split_once('+').unwrap_or((raw, ""));
    let mut parts = core.split('.');
    let mut next = || parts.next()?.parse::<u32>().ok();
    Some(Version {
        major: next()?,
        minor: next().unwrap_or(0),
        patch: next().unwrap_or(0),
        forgejo: meta.starts_with("gitea-"),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub version: Option<Version>,
    pub flavor: Flavor,
    pub logs: Support,
}

/// Versions sondées pour de bon : 10, 11, 12 (adaptateur `Legacy`) et 15 (`Modern`).
pub fn decide(raw: &str) -> Decision {
    let Some(v) = parse(raw) else {
        return Decision {
            version: None,
            flavor: Flavor::Modern,
            logs: Support::Partial(format!(
                "version « {raw} » illisible, comportement supposé récent"
            )),
        };
    };
    let (flavor, logs) = match v.major {
        _ if !v.forgejo && v.major < 7 => (
            Flavor::Legacy,
            Support::Unsupported(format!("{v} : Gitea n'est pas supporté, seulement Forgejo")),
        ),
        0..=9 => (
            Flavor::Legacy,
            Support::Unsupported(format!(
                "Forgejo {v} : les logs n'ont été vérifiés qu'à partir de la 10"
            )),
        ),
        10..=12 => (Flavor::Legacy, Support::Full),
        13 | 14 => (
            Flavor::Modern,
            Support::Partial(format!(
                "Forgejo {v} : version intermédiaire non vérifiée, les logs peuvent ne pas s'afficher"
            )),
        ),
        15 => (Flavor::Modern, Support::Full),
        _ => (
            Flavor::Modern,
            Support::Partial(format!(
                "Forgejo {v} : plus récente que les versions vérifiées (jusqu'à la 15)"
            )),
        ),
    };
    Decision {
        version: Some(v),
        flavor,
        logs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lecture_des_versions() {
        let v = parse("10.0.3+gitea-1.22.0").unwrap();
        assert_eq!((v.major, v.minor, v.patch, v.forgejo), (10, 0, 3, true));
        assert_eq!(parse("v15.0.0").unwrap().major, 15);
        assert_eq!(parse("12").unwrap().to_string(), "12.0.0");
        assert!(!parse("1.22.0").unwrap().forgejo);
        assert_eq!(parse("n'importe quoi"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn table_de_support() {
        let logs = |raw: &str| decide(raw);
        for v in [
            "10.0.3+gitea-1.22.0",
            "11.0.16+gitea-1.22.0",
            "12.0.4+gitea-1.22.0",
        ] {
            let d = logs(v);
            assert_eq!((d.flavor, d.logs), (Flavor::Legacy, Support::Full), "{v}");
        }
        let d = logs("15.0.0+gitea-1.22.0");
        assert_eq!((d.flavor, d.logs), (Flavor::Modern, Support::Full));
        assert!(matches!(
            logs("13.0.1+gitea-1.22.0").logs,
            Support::Partial(_)
        ));
        assert!(matches!(
            logs("14.0.0+gitea-1.22.0").logs,
            Support::Partial(_)
        ));
        assert!(matches!(
            logs("16.0.0+gitea-1.22.0").logs,
            Support::Partial(_)
        ));
        assert!(matches!(
            logs("9.0.3+gitea-1.22.0").logs,
            Support::Unsupported(_)
        ));
        assert!(matches!(
            logs("7.0.12+gitea-1.21.11").logs,
            Support::Unsupported(_)
        ));
        // Gitea (pas de `+gitea-`) et version illisible
        assert!(matches!(logs("1.22.0").logs, Support::Unsupported(ref m) if m.contains("Gitea")));
        assert!(matches!(logs("???").logs, Support::Partial(_)));
    }

    #[test]
    fn particularites_des_adaptateurs() {
        assert!(Flavor::Legacy.needs_csrf() && !Flavor::Legacy.uses_attempts());
        assert!(!Flavor::Modern.needs_csrf() && Flavor::Modern.uses_attempts());
    }
}
