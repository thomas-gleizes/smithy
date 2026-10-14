//! Modèles communs et abstraction `Provider` : l'UI ne connaît que ce crate.

use std::future::Future;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("erreur réseau : {0}")]
    Network(String),
    #[error("non authentifié (token absent ou invalide)")]
    Unauthorized,
    #[error("identifiant ou mot de passe incorrect")]
    BadCredentials,
    #[error("session expirée ou accès refusé : reconnexion nécessaire")]
    SessionExpired,
    #[error("ressource introuvable : {0}")]
    NotFound(String),
    #[error("fonctionnalité non supportée par ce backend : {0}")]
    Unsupported(&'static str),
    /// La version du serveur est connue pour ne pas fonctionner (voir la table de support du backend).
    #[error("version du serveur non supportée : {0}")]
    UnsupportedVersion(String),
    #[error("réponse inattendue : {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Ce qu'une liste de dépôts affiche, sans requête de plus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub repo: Repo,
    pub description: String,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    /// Aucun commit.
    pub empty: bool,
    pub stars: u64,
    pub open_prs: u64,
    pub open_issues: u64,
    /// Dernière mise à jour, en RFC 3339 tel que le serveur la donne.
    pub updated: Option<String>,
    pub default_branch: String,
}

/// D'où vient une liste de dépôts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoSource {
    /// Ceux auxquels on a accès : les siens, ceux d'une organisation, ceux où l'on collabore.
    Mine,
    /// Ceux que l'on suit.
    Watched,
    Starred,
    /// Recherche sur tout le serveur (au plus une page de résultats).
    Search(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Pending,
    Running,
    Success,
    Failure,
    Cancelled,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pipeline {
    pub id: u64,
    /// Numéro du run dans le dépôt (celui des URL), distinct de `id`.
    pub number: u64,
    /// Nom du job ; un run peut en avoir plusieurs, chacun est une entrée.
    pub job: String,
    /// Fichier de workflow (ex. `ci.yml`).
    pub workflow: String,
    /// Titre lisible (message de commit en général).
    pub title: String,
    pub branch: String,
    pub commit: String,
    pub status: Status,
    pub url: Option<String>,
}

/// Une étape d'un job avec sa sortie, déjà nettoyée (voir [`clean_log_line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLog {
    pub name: String,
    pub status: Status,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobLog {
    pub steps: Vec<StepLog>,
    /// Réserve à afficher à côté des logs (ex. version du serveur non vérifiée).
    pub notice: Option<String>,
}

/// Rend une ligne de log affichable dans un terminal : retire les séquences d'échappement
/// (couleurs, curseur…), garde ce qu'une barre de progression laisse après son dernier
/// retour chariot, remplace les tabulations et supprime les autres caractères de contrôle.
pub fn clean_log_line(raw: &str) -> String {
    let raw = raw.trim_end_matches(['\r', '\n']);
    let last = raw.rsplit('\r').next().unwrap_or(raw);
    let mut out = String::with_capacity(last.len());
    let mut chars = last.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI : paramètres puis un octet final entre 0x40 et 0x7E.
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC : jusqu'à BEL ou ESC \.
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' || (n == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    /// Branche par défaut du dépôt.
    pub default: bool,
    /// Dernier commit de la branche, quand le backend le donne dans la liste.
    pub head: Option<BranchHead>,
    /// Branche protégée (poussée directe interdite).
    pub protected: bool,
}

impl Branch {
    pub fn new(name: impl Into<String>, default: bool) -> Self {
        Self {
            name: name.into(),
            default,
            head: None,
            protected: false,
        }
    }
}

/// Le dernier commit d'une branche, tel que la liste des branches le résume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchHead {
    pub sha: String,
    pub message: String,
    pub author: String,
    /// Date RFC 3339 telle que le serveur la donne.
    pub date: Option<String>,
}

/// Un commit de l'historique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    /// Message complet (plusieurs lignes possibles).
    pub message: String,
    pub author: String,
    /// Date RFC 3339 telle que le serveur la donne.
    pub date: Option<String>,
    /// Page web du commit.
    pub url: Option<String>,
}

impl Commit {
    /// Première ligne du message, sans espaces autour.
    pub fn summary(&self) -> &str {
        self.message.lines().next().unwrap_or("").trim()
    }

    /// Les 7 premiers caractères de l'empreinte, comme `git log --oneline`.
    pub fn short_sha(&self) -> &str {
        &self.sha[..self.sha.len().min(7)]
    }
}

/// Une page de l'historique d'une branche, du plus récent au plus ancien.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPage {
    pub commits: Vec<Commit>,
    /// Il peut y avoir des commits plus anciens à la page suivante.
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
    Renamed,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
}

/// Un commit avec ce qu'il change : fichiers, volume et correctif (diff).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    pub commit: Commit,
    pub parents: Vec<String>,
    pub files: Vec<FileChange>,
    pub additions: u64,
    pub deletions: u64,
    /// Correctif au format unifié (`diff --git …`), borné par l'appelant.
    pub patch: String,
    /// Le correctif a été coupé au plafond demandé.
    pub patch_truncated: bool,
}

/// Quelles pull requests lister.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PullFilter {
    #[default]
    Open,
    /// Fermées, fusionnées ou non.
    Closed,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullState {
    Open,
    Merged,
    /// Fermée sans fusion.
    Closed,
}

/// Une pull request, telle que la liste la donne (déjà complète : la liste suffit à l'afficher).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    /// Description (Markdown), peut être vide.
    pub body: String,
    pub author: String,
    pub state: PullState,
    pub draft: bool,
    /// Branche proposée et branche visée.
    pub head: String,
    pub base: String,
    /// Commit de tête de la branche proposée (pour son statut de CI).
    pub head_sha: String,
    pub labels: Vec<String>,
    /// Commentaires de la conversation (hors revues).
    pub comments: u64,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    /// `None` : inconnu (calcul en cours côté serveur, ou PR fermée).
    pub mergeable: Option<bool>,
    /// Dates RFC 3339 telles que le serveur les donne.
    pub created: Option<String>,
    pub updated: Option<String>,
    /// Fusion ou fermeture.
    pub closed: Option<String>,
    pub url: Option<String>,
}

/// Une page de la liste des pull requests, de la plus récente à la plus ancienne.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPage {
    pub pulls: Vec<PullRequest>,
    /// Il peut y en avoir d'autres à la page suivante.
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    /// Demandée, pas encore rendue.
    Requested,
    Other,
}

/// Un commentaire de ligne d'une revue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub path: String,
    pub body: String,
    /// Dernière ligne du morceau de diff commenté (celle visée par le commentaire).
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub author: String,
    pub state: ReviewState,
    pub body: String,
    pub date: Option<String>,
    /// Écartée (rejetée, ou remplacée par une revue plus récente du même relecteur).
    pub dismissed: bool,
    /// Rendue sur un commit qui n'est plus la tête de la branche.
    pub stale: bool,
    pub comments: Vec<ReviewComment>,
}

/// Un commentaire de la conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub date: Option<String>,
}

/// Un statut de CI posé sur un commit (`context` : son nom, ex. `ci/build`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckStatus {
    pub context: String,
    pub status: Status,
    pub description: String,
    pub url: Option<String>,
}

/// Une pull request avec tout ce qu'il faut pour la lire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullDetail {
    pub pull: PullRequest,
    pub commits: Vec<Commit>,
    /// Il y a plus de commits que ceux listés.
    pub commits_truncated: bool,
    pub reviews: Vec<Review>,
    /// `None` : le token n'a pas le droit de lire la conversation.
    pub comments: Option<Vec<Comment>>,
    /// Statut de CI combiné de la tête (`None` : aucun statut posé) et le détail.
    pub checks: Option<Status>,
    pub statuses: Vec<CheckStatus>,
    /// Correctif complet au format unifié, borné par l'appelant.
    pub patch: String,
    pub patch_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// Lien symbolique : affiché, pas ouvert.
    Symlink,
    /// Sous-module : affiché, pas ouvert.
    Submodule,
}

/// Une entrée d'un dossier du dépôt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Chemin depuis la racine du dépôt.
    pub path: String,
    pub kind: EntryKind,
    /// Octets ; 0 pour un dossier.
    pub size: u64,
}

/// Contenu d'un fichier, lu avec un plafond d'octets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContent {
    /// Taille réelle du fichier si le serveur l'a annoncée, sinon les octets lus.
    pub size: u64,
    /// `None` pour un fichier binaire (octet nul dans les premiers Kio).
    pub text: Option<String>,
    /// Seul le début a été lu (le plafond est atteint).
    pub truncated: bool,
}

/// Rend une ligne de fichier affichable sans en perdre le contenu : retire le `\r` final d'une fin
/// de ligne Windows, étend les tabulations, et montre chaque autre caractère de contrôle par son
/// image Unicode (`\x1b` devient `␛`) au lieu de l'avaler ou de le laisser agir sur le terminal.
/// À ne pas confondre avec [`clean_log_line`], qui garde ce qui suit le dernier `\r` (barres de
/// progression) et supprimerait du contenu d'un fichier.
pub fn clean_text_line(raw: &str) -> String {
    let raw = raw.strip_suffix('\r').unwrap_or(raw);
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\t' => out.push_str("    "),
            '\x7f' => out.push('\u{2421}'),
            c if (c as u32) < 0x20 => out.push(char::from_u32(0x2400 + c as u32).unwrap_or('?')),
            // Contrôles C1 (0x80-0x9f) : certains terminaux les interprètent.
            c if ('\u{80}'..='\u{9f}').contains(&c) => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

/// Fonctionnalités optionnelles qu'un backend peut ou non offrir.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub logs: bool,
    /// Branches, dossiers et fichiers du dépôt (l'explorateur), historique et pull requests.
    pub browse: bool,
}

pub trait Provider: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn list_repos(&self, source: &RepoSource)
    -> impl Future<Output = Result<Vec<RepoInfo>>> + Send;
    fn repo_info(&self, repo: &Repo) -> impl Future<Output = Result<RepoInfo>> + Send;
    fn list_pipelines(&self, repo: &Repo) -> impl Future<Output = Result<Vec<Pipeline>>> + Send;

    /// Branches du dépôt, la par défaut en premier. Réservé aux backends dont
    /// `capabilities().browse` est vrai, comme [`Provider::read_dir`] et [`Provider::read_file`].
    fn list_branches(&self, _repo: &Repo) -> impl Future<Output = Result<Vec<Branch>>> + Send {
        async { Err(Error::Unsupported("explorateur")) }
    }

    /// Une page de l'historique d'une branche (`reference` vide = branche par défaut), du plus
    /// récent au plus ancien ; `page` commence à 1. Réservé aux backends dont
    /// `capabilities().browse` est vrai.
    fn list_commits(
        &self,
        _repo: &Repo,
        _reference: &str,
        _page: u32,
    ) -> impl Future<Output = Result<CommitPage>> + Send {
        async { Err(Error::Unsupported("historique")) }
    }

    /// Un commit avec ses fichiers et son correctif (lu sur au plus `max_bytes` octets).
    fn commit_detail(
        &self,
        _repo: &Repo,
        _sha: &str,
        _max_bytes: usize,
    ) -> impl Future<Output = Result<CommitDetail>> + Send {
        async { Err(Error::Unsupported("historique")) }
    }

    /// Une page des pull requests du dépôt (`page` commence à 1). Réservé aux backends dont
    /// `capabilities().browse` est vrai.
    fn list_pulls(
        &self,
        _repo: &Repo,
        _filter: PullFilter,
        _page: u32,
    ) -> impl Future<Output = Result<PullPage>> + Send {
        async { Err(Error::Unsupported("pull requests")) }
    }

    /// Une pull request : commits, revues, conversation, statuts de CI et correctif (lu sur au
    /// plus `max_bytes` octets).
    fn pull_detail(
        &self,
        _repo: &Repo,
        _number: u64,
        _max_bytes: usize,
    ) -> impl Future<Output = Result<PullDetail>> + Send {
        async { Err(Error::Unsupported("pull requests")) }
    }

    /// Contenu d'un dossier (`path` vide = racine) à une référence (branche, étiquette, commit).
    fn read_dir(
        &self,
        _repo: &Repo,
        _reference: &str,
        _path: &str,
    ) -> impl Future<Output = Result<Vec<Entry>>> + Send {
        async { Err(Error::Unsupported("explorateur")) }
    }

    /// Un fichier, lu sur au plus `max_bytes` octets.
    fn read_file(
        &self,
        _repo: &Repo,
        _reference: &str,
        _path: &str,
        _max_bytes: usize,
    ) -> impl Future<Output = Result<FileContent>> + Send {
        async { Err(Error::Unsupported("explorateur")) }
    }

    /// Étapes et sortie du job de `pipeline`. Réservé aux backends dont
    /// `capabilities().logs` est vrai.
    fn job_log(
        &self,
        _repo: &Repo,
        _pipeline: &Pipeline,
    ) -> impl Future<Output = Result<JobLog>> + Send {
        async { Err(Error::Unsupported("logs")) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nettoyage_des_lignes_de_fichier() {
        assert_eq!(clean_text_line("ligne\r"), "ligne", "fin de ligne Windows");
        assert_eq!(clean_text_line("a\tb"), "a    b");
        // Aucun contenu perdu : les contrôles deviennent visibles
        assert_eq!(
            clean_text_line("x\ry"),
            "x\u{240d}y",
            "\\r au milieu d'une ligne conservé"
        );
        assert_eq!(
            clean_text_line("\x1b[31mrouge\x1b[0m"),
            "\u{241b}[31mrouge\u{241b}[0m"
        );
        assert_eq!(
            clean_text_line("a\0b\x07\x7f"),
            "a\u{2400}b\u{2407}\u{2421}"
        );
        // Unicode, accents et espaces intacts
        assert_eq!(clean_text_line("été — 日本語  "), "été — 日本語  ");
        assert_eq!(clean_text_line(""), "");
        assert_eq!(clean_text_line("\u{85}"), "\u{fffd}", "contrôle C1");
    }

    #[test]
    fn nettoyage_des_lignes_de_log() {
        // Couleurs
        assert_eq!(
            clean_log_line("\x1b[32mOK\x1b[0m compilation"),
            "OK compilation"
        );
        assert_eq!(clean_log_line("\x1b[1;33mjaune\x1b[0m"), "jaune");
        // Barre de progression : seul l'état final reste
        assert_eq!(clean_log_line("10%\r50%\r100%"), "100%");
        assert_eq!(clean_log_line("texte\r"), "texte");
        assert_eq!(clean_log_line("a\r\n"), "a");
        // Déplacement du curseur, titre de fenêtre (OSC, BEL et ST)
        assert_eq!(clean_log_line("\x1b[2K\x1b[1Gligne"), "ligne");
        assert_eq!(clean_log_line("\x1b]0;titre\x07visible"), "visible");
        assert_eq!(clean_log_line("\x1b]0;titre\x1b\\visible"), "visible");
        // Tabulation et caractères de contrôle
        assert_eq!(clean_log_line("a\tb"), "a    b");
        assert_eq!(clean_log_line("a\x08\x07b\x7f"), "ab");
        // Unicode et séquence tronquée en fin de ligne
        assert_eq!(clean_log_line("🏁  Job succeeded"), "🏁  Job succeeded");
        assert_eq!(clean_log_line("fin\x1b[3"), "fin");
        assert_eq!(clean_log_line("fin\x1b"), "fin");
    }

    #[test]
    fn full_name() {
        let r = Repo {
            owner: "thomas".into(),
            name: "smithy".into(),
        };
        assert_eq!(r.full_name(), "thomas/smithy");
    }
}
