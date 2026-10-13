//! État de l'application : aucune dépendance au terminal, donc testable seul.

use smithy_core::{Pipeline, Repo};

use crate::session::Session;

pub enum Msg {
    /// Résultat du login : la session à conserver, ou le message d'erreur.
    LoggedIn(Result<Session, String>),
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoginField {
    Username,
    Password,
}

/// Formulaire de connexion. Le mot de passe n'y reste que le temps de la saisie.
#[derive(Debug)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
    pub field: LoginField,
    pub error: Option<String>,
    pub busy: bool,
}

pub struct App {
    pub repos: Vec<Repo>,
    pub pipelines: Vec<Pipeline>,
    pub repo_idx: usize,
    pub pipe_idx: usize,
    pub focus: Focus,
    pub error: Option<String>,
    pub should_quit: bool,
    /// Formulaire affiché par-dessus l'interface tant que l'utilisateur n'est pas connecté.
    pub login: Option<LoginForm>,
    /// Nom et URL du serveur, pour le titre du formulaire.
    pub server_label: String,
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
            login: None,
            server_label: String::new(),
        }
    }

    /// Ouvre le formulaire ; si l'identifiant est connu, le curseur démarre sur le mot de passe.
    pub fn open_login(&mut self, username: &str) {
        self.login = Some(LoginForm {
            username: username.to_owned(),
            password: String::new(),
            field: if username.is_empty() {
                LoginField::Username
            } else {
                LoginField::Password
            },
            error: None,
            busy: false,
        });
    }

    pub fn login_type(&mut self, c: char) {
        if let Some(f) = self.login.as_mut().filter(|f| !f.busy) {
            match f.field {
                LoginField::Username => f.username.push(c),
                LoginField::Password => f.password.push(c),
            }
        }
    }

    pub fn login_backspace(&mut self) {
        if let Some(f) = self.login.as_mut().filter(|f| !f.busy) {
            match f.field {
                LoginField::Username => f.username.pop(),
                LoginField::Password => f.password.pop(),
            };
        }
    }

    pub fn login_toggle_field(&mut self) {
        if let Some(f) = self.login.as_mut() {
            f.field = match f.field {
                LoginField::Username => LoginField::Password,
                LoginField::Password => LoginField::Username,
            };
        }
    }

    /// Entrée : passe au mot de passe, ou valide. Renvoie `(identifiant, mot de passe)`
    /// à envoyer ; le mot de passe est alors retiré du formulaire.
    pub fn login_enter(&mut self) -> Option<(String, String)> {
        let f = self.login.as_mut().filter(|f| !f.busy)?;
        if f.field == LoginField::Username && !f.username.is_empty() {
            f.field = LoginField::Password;
            return None;
        }
        if f.username.is_empty() || f.password.is_empty() {
            f.error = Some("identifiant et mot de passe requis".to_owned());
            return None;
        }
        f.busy = true;
        f.error = None;
        Some((f.username.clone(), std::mem::take(&mut f.password)))
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
            Msg::LoggedIn(Ok(_)) => {
                self.login = None;
                self.error = None;
                false
            }
            Msg::LoggedIn(Err(e)) => {
                if let Some(f) = self.login.as_mut() {
                    f.busy = false;
                    f.field = LoginField::Password;
                    f.error = Some(e);
                }
                false
            }
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

    fn typed(app: &mut App, text: &str) {
        text.chars().for_each(|c| app.login_type(c));
    }

    #[test]
    fn formulaire_saisie_et_envoi() {
        let mut a = App::new();
        a.open_login("");
        typed(&mut a, "thomas");
        assert!(a.login_enter().is_none()); // passe au mot de passe
        assert_eq!(a.login.as_ref().unwrap().field, LoginField::Password);
        typed(&mut a, "secrex");
        a.login_backspace();
        a.login_type('t');
        let sent = a.login_enter().unwrap();
        assert_eq!(sent, ("thomas".to_owned(), "secret".to_owned()));
        let f = a.login.as_ref().unwrap();
        assert!(
            f.busy && f.password.is_empty(),
            "mot de passe retiré du formulaire"
        );
        // Pendant l'envoi : saisie ignorée, pas de double envoi.
        a.login_type('x');
        assert!(a.login.as_ref().unwrap().password.is_empty());
        assert!(a.login_enter().is_none());
    }

    #[test]
    fn identifiant_connu_demarre_sur_le_mot_de_passe() {
        let mut a = App::new();
        a.open_login("thomas");
        assert_eq!(a.login.as_ref().unwrap().field, LoginField::Password);
    }

    #[test]
    fn champs_vides_refuses() {
        let mut a = App::new();
        a.open_login("thomas");
        assert!(a.login_enter().is_none());
        let f = a.login.as_ref().unwrap();
        assert!(!f.busy && f.error.is_some());
    }

    #[test]
    fn echec_de_login_rouvre_la_saisie() {
        let mut a = App::new();
        a.open_login("thomas");
        typed(&mut a, "mauvais");
        a.login_enter().unwrap();
        a.apply(Msg::LoggedIn(Err(
            "identifiant ou mot de passe incorrect".into()
        )));
        let f = a.login.as_ref().unwrap();
        assert!(!f.busy);
        assert_eq!(f.field, LoginField::Password);
        assert!(f.error.as_deref().unwrap().contains("incorrect"));
    }

    #[test]
    fn succes_de_login_ferme_le_formulaire() {
        let mut a = App::new();
        a.open_login("thomas");
        let session = Session {
            url: "http://a".into(),
            username: "thomas".into(),
            token: Some("t".into()),
            cookies: Default::default(),
        };
        a.apply(Msg::LoggedIn(Ok(session)));
        assert!(a.login.is_none());
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
