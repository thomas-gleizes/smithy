//! État de l'application : aucune dépendance au terminal, donc testable seul.

use smithy_core::{Pipeline, Repo};

pub enum Msg {
    Repos(Result<Vec<Repo>, String>),
    Pipelines {
        repo: Repo,
        result: Result<Vec<Pipeline>, String>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Repos,
    Pipelines,
}

pub struct App {
    pub repos: Vec<Repo>,
    pub pipelines: Vec<Pipeline>,
    pub repo_idx: usize,
    pub pipe_idx: usize,
    pub focus: Focus,
    pub error: Option<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            repos: Vec::new(),
            pipelines: Vec::new(),
            repo_idx: 0,
            pipe_idx: 0,
            focus: Focus::Repos,
            error: None,
            should_quit: false,
        }
    }

    pub fn selected_repo(&self) -> Option<&Repo> {
        self.repos.get(self.repo_idx)
    }

    pub fn selected_pipeline(&self) -> Option<&Pipeline> {
        self.pipelines.get(self.pipe_idx)
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Repos => Focus::Pipelines,
            Focus::Pipelines => Focus::Repos,
        };
    }

    /// Déplace la sélection de la liste active. Renvoie `true` si le dépôt
    /// sélectionné a changé (il faut alors recharger ses pipelines).
    pub fn step(&mut self, delta: isize) -> bool {
        match self.focus {
            Focus::Repos => {
                let new = clamp_step(self.repo_idx, self.repos.len(), delta);
                let changed = new != self.repo_idx;
                if changed {
                    self.repo_idx = new;
                    self.pipelines.clear();
                    self.pipe_idx = 0;
                }
                changed
            }
            Focus::Pipelines => {
                self.pipe_idx = clamp_step(self.pipe_idx, self.pipelines.len(), delta);
                false
            }
        }
    }

    /// Applique un résultat réseau. Renvoie `true` s'il faut charger les
    /// pipelines du dépôt sélectionné.
    pub fn apply(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Repos(Ok(repos)) => {
                self.error = None;
                let previous = self.selected_repo().cloned();
                self.repos = repos;
                self.repo_idx = previous
                    .and_then(|p| self.repos.iter().position(|r| *r == p))
                    .unwrap_or(0);
                !self.repos.is_empty()
            }
            Msg::Repos(Err(e)) => {
                self.error = Some(e);
                false
            }
            Msg::Pipelines { repo, result } => {
                // Réponse périmée : l'utilisateur a changé de dépôt entre-temps.
                if self.selected_repo() != Some(&repo) {
                    return false;
                }
                match result {
                    Ok(p) => {
                        self.error = None;
                        self.pipelines = p;
                        self.pipe_idx = self.pipe_idx.min(self.pipelines.len().saturating_sub(1));
                    }
                    Err(e) => self.error = Some(e),
                }
                false
            }
        }
    }
}

fn clamp_step(idx: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    idx.saturating_add_signed(delta).min(len - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_core::Status;

    fn repo(n: &str) -> Repo {
        Repo {
            owner: "o".into(),
            name: n.into(),
        }
    }

    fn pipe(id: u64) -> Pipeline {
        Pipeline {
            id,
            workflow: "ci.yml".into(),
            title: "t".into(),
            branch: "main".into(),
            commit: "abc".into(),
            status: Status::Success,
            url: None,
        }
    }

    #[test]
    fn selection_bornee() {
        let mut a = App::new();
        assert!(!a.step(1)); // liste vide
        a.apply(Msg::Repos(Ok(vec![repo("a"), repo("b")])));
        assert!(!a.step(-1)); // déjà en haut
        assert!(a.step(1));
        assert!(!a.step(1)); // déjà en bas
        assert_eq!(a.repo_idx, 1);
    }

    #[test]
    fn reponse_perimee_ignoree() {
        let mut a = App::new();
        a.apply(Msg::Repos(Ok(vec![repo("a"), repo("b")])));
        a.step(1);
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![pipe(1)]),
        });
        assert!(a.pipelines.is_empty());
        a.apply(Msg::Pipelines {
            repo: repo("b"),
            result: Ok(vec![pipe(2)]),
        });
        assert_eq!(a.pipelines.len(), 1);
    }

    #[test]
    fn rechargement_conserve_la_selection() {
        let mut a = App::new();
        a.apply(Msg::Repos(Ok(vec![repo("a"), repo("b")])));
        a.step(1);
        let fetch = a.apply(Msg::Repos(Ok(vec![repo("z"), repo("a"), repo("b")])));
        assert!(fetch);
        assert_eq!(a.selected_repo(), Some(&repo("b")));
    }

    #[test]
    fn erreur_puis_succes_efface_le_bandeau() {
        let mut a = App::new();
        a.apply(Msg::Repos(Err("boum".into())));
        assert_eq!(a.error.as_deref(), Some("boum"));
        a.apply(Msg::Repos(Ok(vec![repo("a")])));
        assert!(a.error.is_none());
    }

    #[test]
    fn index_pipeline_reborne_apres_rafraichissement() {
        let mut a = App::new();
        a.apply(Msg::Repos(Ok(vec![repo("a")])));
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![pipe(1), pipe(2), pipe(3)]),
        });
        a.focus = Focus::Pipelines;
        a.step(2);
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![pipe(1)]),
        });
        assert_eq!(a.pipe_idx, 0);
    }
}
