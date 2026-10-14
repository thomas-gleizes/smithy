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
