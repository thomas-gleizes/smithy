//! Ce que l'état de l'application demande à la boucle principale de lancer. L'état reste pur :
//! il dépose des requêtes, la boucle les exécute, les réponses reviennent sous forme de `Msg`.

use smithy_core::{Pipeline, Repo, RepoSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Repos(RepoSource),
    RepoInfo(Repo),
    Pipelines(Repo),
    Logs {
        repo: Repo,
        pipeline: Pipeline,
    },
    Branches(Repo),
    /// Une page de l'historique d'une branche (`reference` vide : la branche par défaut).
    Commits {
        repo: Repo,
        reference: String,
        page: u32,
    },
    CommitDetail {
        repo: Repo,
        sha: String,
    },
    Dir {
        repo: Repo,
        reference: String,
        path: String,
    },
    File {
        repo: Repo,
        reference: String,
        path: String,
    },
}
