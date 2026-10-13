//! Modèles communs et abstraction `Provider` : l'UI ne connaît que ce crate.

use std::future::Future;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("erreur réseau : {0}")]
    Network(String),
    #[error("non authentifié (token absent ou invalide)")]
    Unauthorized,
    #[error("ressource introuvable : {0}")]
    NotFound(String),
    #[error("fonctionnalité non supportée par ce backend : {0}")]
    Unsupported(&'static str),
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
    /// Fichier de workflow (ex. `ci.yml`).
    pub workflow: String,
    /// Titre lisible (message de commit en général).
    pub title: String,
    pub branch: String,
    pub commit: String,
    pub status: Status,
    pub url: Option<String>,
}

/// Fonctionnalités optionnelles qu'un backend peut ou non offrir.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub logs: bool,
}

pub trait Provider: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn list_repos(&self) -> impl Future<Output = Result<Vec<Repo>>> + Send;
    fn list_pipelines(&self, repo: &Repo) -> impl Future<Output = Result<Vec<Pipeline>>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_name() {
        let r = Repo {
            owner: "thomas".into(),
            name: "smithy".into(),
        };
        assert_eq!(r.full_name(), "thomas/smithy");
    }
}
