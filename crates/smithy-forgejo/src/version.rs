//! Version du serveur → adaptateur à utiliser et niveau de support.
//!
//! Les routes web qui servent les logs changent d'une version de Forgejo à l'autre
//! (nom du cookie de session, jeton CSRF, segment `attempt`…). Plutôt qu'un code unique
//! qui suppose la dernière version, chaque famille de versions a un adaptateur
//! ([`Flavor`]) et une entrée dans la table ci-dessous. Seules les versions réellement
//! sondées (`dev/dev.sh probe`, voir `dev/probe-logs.py` : 10, 11, 12, 13, 14, 15) sont
//! déclarées complètes ; les autres sont partielles (on essaie, en le disant) ou non supportées.

/// Famille de comportements des routes web. Deux choses varient d'une version à l'autre,
/// indépendamment (mesuré avec `dev/dev.sh probe`) : la protection CSRF de la route et le
/// segment `attempt/{n}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// Forgejo 10 à 12 : CSRF exigé (cookie `_csrf` + en-tête), segment `attempt` absent (404).
    Legacy,
    /// Forgejo 13 : CSRF encore exigé, mais `attempt/{n}` devient obligatoire.
    Transitional,
    /// Forgejo 14 et 15 : plus de CSRF sur cette route, `attempt/{n}` obligatoire.
    Modern,
}

impl Flavor {
    pub(crate) fn needs_csrf(self) -> bool {
        matches!(self, Flavor::Legacy | Flavor::Transitional)
    }

    pub(crate) fn uses_attempts(self) -> bool {
        matches!(self, Flavor::Transitional | Flavor::Modern)
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

/// Versions sondées pour de bon : 10 à 15.
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
        13 => (Flavor::Transitional, Support::Full),
        14 | 15 => (Flavor::Modern, Support::Full),
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
        let full = |raw: &str, flavor: Flavor| {
            let d = decide(raw);
            assert_eq!((d.flavor, d.logs), (flavor, Support::Full), "{raw}");
        };
        for v in ["10.0.3", "11.0.16", "12.0.4"] {
            full(&format!("{v}+gitea-1.22.0"), Flavor::Legacy);
        }
        full("13.0.5+gitea-1.22.0", Flavor::Transitional);
        full("14.0.5+gitea-1.22.0", Flavor::Modern);
        full("15.0.10+gitea-1.22.0", Flavor::Modern);
        assert!(matches!(
            decide("16.0.0+gitea-1.22.0").logs,
            Support::Partial(_)
        ));
        assert!(matches!(
            decide("9.0.3+gitea-1.22.0").logs,
            Support::Unsupported(_)
        ));
        assert!(matches!(
            decide("7.0.12+gitea-1.21.11").logs,
            Support::Unsupported(_)
        ));
        // Gitea (pas de `+gitea-`) et version illisible
        assert!(
            matches!(decide("1.22.0").logs, Support::Unsupported(ref m) if m.contains("Gitea"))
        );
        assert!(matches!(decide("???").logs, Support::Partial(_)));
    }

    #[test]
    fn particularites_des_adaptateurs() {
        let flags = |f: Flavor| (f.needs_csrf(), f.uses_attempts());
        assert_eq!(flags(Flavor::Legacy), (true, false));
        assert_eq!(flags(Flavor::Transitional), (true, true));
        assert_eq!(flags(Flavor::Modern), (false, true));
    }
}
