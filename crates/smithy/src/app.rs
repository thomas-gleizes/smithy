//! État de l'application : aucune dépendance au terminal, donc testable seul.

use std::cell::Cell;

use smithy_core::{Error, JobLog, Pipeline, Repo, Status};

use crate::session::Session;

pub enum Msg {
    /// Logs d'un job ; `pipeline_id` sert à ignorer une réponse périmée.
    Logs {
        repo: Repo,
        pipeline_id: u64,
        result: Result<JobLog, Error>,
    },
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
pub enum LineKind {
    /// Titre d'étape, coloré selon son statut.
    Step(Status),
    Text,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LogLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug)]
pub enum LogState {
    Loading,
    Ready(Vec<LogLine>),
    Failed(String),
}

/// Vue plein écran de la sortie d'un job.
#[derive(Debug)]
pub struct LogView {
    pub repo: Repo,
    pub pipeline: Pipeline,
    pub state: LogState,
    /// Première ligne affichée.
    pub scroll: usize,
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
    /// Vue des logs ouverte sur le job sélectionné, le cas échéant.
    pub logs: Option<LogView>,
    /// Le backend sait-il fournir des logs ? (`Capabilities::logs`)
    pub logs_supported: bool,
    /// Hauteur utile de la vue des logs, écrite par le rendu qui seul la connaît.
    pub viewport: Cell<usize>,
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
            logs: None,
            logs_supported: false,
            viewport: Cell::new(20),
        }
    }

    /// Entrée sur un run : ouvre ses logs. Sur la liste des dépôts, passe aux runs.
    /// Renvoie la demande à envoyer au backend.
    pub fn open_logs(&mut self) -> Option<(Repo, Pipeline)> {
        if self.focus == Focus::Repos {
            self.focus = Focus::Pipelines;
            return None;
        }
        if !self.logs_supported {
            self.error = Some("ce backend ne fournit pas les logs".to_owned());
            return None;
        }
        let repo = self.selected_repo()?.clone();
        let pipeline = self.selected_pipeline()?.clone();
        self.logs = Some(LogView {
            repo: repo.clone(),
            pipeline: pipeline.clone(),
            state: LogState::Loading,
            scroll: 0,
        });
        Some((repo, pipeline))
    }

    pub fn close_logs(&mut self) {
        self.logs = None;
    }

    /// Redemande les logs affichés. Le contenu déjà là reste visible pendant ce temps ;
    /// une erreur précédente repasse en chargement.
    pub fn refetch_logs(&mut self) -> Option<(Repo, Pipeline)> {
        let v = self.logs.as_mut()?;
        if matches!(v.state, LogState::Failed(_)) {
            v.state = LogState::Loading;
        }
        Some((v.repo.clone(), v.pipeline.clone()))
    }

    /// Le job affiché tourne encore : ses logs sont à rafraîchir périodiquement.
    pub fn logs_are_live(&self) -> bool {
        let Some(v) = &self.logs else { return false };
        let current = self
            .pipelines
            .iter()
            .find(|p| p.id == v.pipeline.id)
            .map_or(v.pipeline.status, |p| p.status);
        matches!(current, Status::Running | Status::Pending)
    }

    fn logs_max_scroll(&self) -> usize {
        self.logs
            .as_ref()
            .map_or(0, |v| max_scroll(&v.state, self.viewport.get().max(1)))
    }

    pub fn logs_scroll(&mut self, delta: isize) {
        let max = self.logs_max_scroll();
        if let Some(v) = self.logs.as_mut() {
            v.scroll = v.scroll.saturating_add_signed(delta).min(max);
        }
    }

    pub fn logs_page(&mut self, pages: isize) {
        let page = self.viewport.get().saturating_sub(1).max(1) as isize;
        self.logs_scroll(pages * page);
    }

    pub fn logs_top(&mut self) {
        if let Some(v) = self.logs.as_mut() {
            v.scroll = 0;
        }
    }

    pub fn logs_bottom(&mut self) {
        let max = self.logs_max_scroll();
        if let Some(v) = self.logs.as_mut() {
            v.scroll = max;
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
            Msg::Logs {
                repo,
                pipeline_id,
                result,
            } => {
                // Réponse périmée : la vue a été fermée ou ouverte sur un autre job.
                let current = self
                    .logs
                    .as_ref()
                    .is_some_and(|v| v.repo == repo && v.pipeline.id == pipeline_id);
                if !current {
                    return false;
                }
                match result {
                    Ok(log) => {
                        let live = self.logs_are_live();
                        let lines = flatten(&log);
                        let viewport = self.viewport.get().max(1);
                        let Some(v) = self.logs.as_mut() else {
                            return false;
                        };
                        let first_load = matches!(v.state, LogState::Loading);
                        let was_at_bottom = v.scroll >= max_scroll(&v.state, viewport);
                        let target = if first_load {
                            initial_scroll(&lines, live)
                        } else if live && was_at_bottom {
                            usize::MAX // on suit la fin d'un job en cours
                        } else {
                            v.scroll
                        };
                        v.state = LogState::Ready(lines);
                        v.scroll = target.min(max_scroll(&v.state, viewport));
                    }
                    Err(e) => {
                        let ready = self
                            .logs
                            .as_ref()
                            .is_some_and(|v| matches!(v.state, LogState::Ready(_)));
                        if ready {
                            // On garde ce qu'on affiche, l'échec va dans le bandeau.
                            self.error = Some(e.to_string());
                        } else if let Some(v) = self.logs.as_mut() {
                            v.state = LogState::Failed(e.to_string());
                        }
                    }
                }
                false
            }
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

/// Titres d'étape et lignes de sortie à plat, pour un défilement simple.
fn flatten(log: &JobLog) -> Vec<LogLine> {
    if log.steps.is_empty() {
        return vec![LogLine {
            kind: LineKind::Text,
            text: "(aucune étape : le job n'a pas démarré)".to_owned(),
        }];
    }
    let mut out = Vec::new();
    for step in &log.steps {
        out.push(LogLine {
            kind: LineKind::Step(step.status),
            text: step.name.clone(),
        });
        out.extend(step.lines.iter().map(|l| LogLine {
            kind: LineKind::Text,
            text: l.clone(),
        }));
    }
    out
}

fn max_scroll(state: &LogState, viewport: usize) -> usize {
    match state {
        LogState::Ready(lines) => lines.len().saturating_sub(viewport),
        _ => 0,
    }
}

/// Où se placer à l'ouverture : sur la première étape en échec (c'est là qu'on cherche),
/// sinon en bas si le job tourne encore, sinon en haut.
fn initial_scroll(lines: &[LogLine], live: bool) -> usize {
    if let Some(i) = lines
        .iter()
        .position(|l| l.kind == LineKind::Step(Status::Failure))
    {
        i
    } else if live {
        usize::MAX
    } else {
        0
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
            number: id,
            job: "build".into(),
            workflow: "ci.yml".into(),
            title: "t".into(),
            branch: "main".into(),
            commit: "abc".into(),
            status: Status::Success,
            url: None,
        }
    }

    fn step(name: &str, status: Status, n: usize) -> smithy_core::StepLog {
        smithy_core::StepLog {
            name: name.into(),
            status,
            lines: (1..=n).map(|i| format!("{name} {i}")).collect(),
        }
    }

    /// App avec un dépôt, un run sélectionné (id 7) et les logs pris en charge.
    fn app_on_run(status: Status) -> App {
        let mut a = App::new();
        a.logs_supported = true;
        a.apply(Msg::Repos(Ok(vec![repo("a")])));
        let mut p = pipe(7);
        p.status = status;
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![p]),
        });
        a.focus = Focus::Pipelines;
        a.viewport.set(5);
        a
    }

    fn deliver(a: &mut App, id: u64, result: Result<JobLog, Error>) {
        a.apply(Msg::Logs {
            repo: repo("a"),
            pipeline_id: id,
            result,
        });
    }

    fn lines(a: &App) -> &Vec<LogLine> {
        match &a.logs.as_ref().unwrap().state {
            LogState::Ready(l) => l,
            s => panic!("pas prêt : {s:?}"),
        }
    }

    #[test]
    fn entree_sur_les_depots_passe_aux_runs() {
        let mut a = app_on_run(Status::Success);
        a.focus = Focus::Repos;
        assert!(a.open_logs().is_none());
        assert_eq!(a.focus, Focus::Pipelines);
        assert!(a.logs.is_none());
    }

    #[test]
    fn ouverture_des_logs() {
        let mut a = app_on_run(Status::Success);
        let (r, p) = a.open_logs().unwrap();
        assert_eq!((r, p.id), (repo("a"), 7));
        assert!(matches!(a.logs.as_ref().unwrap().state, LogState::Loading));

        let mut b = app_on_run(Status::Success);
        b.logs_supported = false;
        assert!(b.open_logs().is_none() && b.logs.is_none() && b.error.is_some());

        let mut c = App::new(); // aucun run sélectionné
        c.logs_supported = true;
        c.focus = Focus::Pipelines;
        assert!(c.open_logs().is_none());
    }

    #[test]
    fn ouverture_sur_la_premiere_etape_en_echec() {
        let mut a = app_on_run(Status::Failure);
        a.open_logs();
        let log = JobLog {
            steps: vec![
                step("Set up job", Status::Success, 9),
                step("cargo test", Status::Failure, 30),
                step("Complete job", Status::Failure, 2),
            ],
        };
        deliver(&mut a, 7, Ok(log));
        // 1 titre + 9 lignes, puis le titre de l'étape en échec à l'index 10
        assert_eq!(a.logs.as_ref().unwrap().scroll, 10);
        assert_eq!(lines(&a)[10].kind, LineKind::Step(Status::Failure));
        assert_eq!(lines(&a).len(), 1 + 9 + 1 + 30 + 1 + 2);
    }

    #[test]
    fn job_reussi_ouvert_en_haut_job_en_cours_en_bas() {
        let log = || JobLog {
            steps: vec![step("build", Status::Success, 20)],
        };
        let mut done = app_on_run(Status::Success);
        done.open_logs();
        deliver(&mut done, 7, Ok(log()));
        assert_eq!(done.logs.as_ref().unwrap().scroll, 0);

        let mut live = app_on_run(Status::Running);
        live.open_logs();
        deliver(&mut live, 7, Ok(log()));
        assert_eq!(live.logs.as_ref().unwrap().scroll, 21 - 5);
    }

    #[test]
    fn job_en_cours_suit_la_fin_sauf_si_on_a_remonte() {
        let mut a = app_on_run(Status::Running);
        a.open_logs();
        let log = |n| JobLog {
            steps: vec![step("build", Status::Running, n)],
        };
        deliver(&mut a, 7, Ok(log(20)));
        deliver(&mut a, 7, Ok(log(30))); // on était en bas : on suit
        assert_eq!(a.logs.as_ref().unwrap().scroll, 31 - 5);
        a.logs_scroll(-3);
        let pos = a.logs.as_ref().unwrap().scroll;
        deliver(&mut a, 7, Ok(log(40))); // on a remonté : on reste
        assert_eq!(a.logs.as_ref().unwrap().scroll, pos);
    }

    #[test]
    fn reponse_de_logs_perimee_ignoree() {
        let mut a = app_on_run(Status::Success);
        deliver(&mut a, 7, Ok(JobLog::default())); // vue fermée
        assert!(a.logs.is_none());
        a.open_logs();
        deliver(&mut a, 8, Ok(JobLog::default())); // autre run
        assert!(matches!(a.logs.as_ref().unwrap().state, LogState::Loading));
        a.apply(Msg::Logs {
            repo: repo("autre"),
            pipeline_id: 7,
            result: Ok(JobLog::default()),
        });
        assert!(matches!(a.logs.as_ref().unwrap().state, LogState::Loading));
    }

    #[test]
    fn job_sans_etape() {
        let mut a = app_on_run(Status::Cancelled);
        a.open_logs();
        deliver(&mut a, 7, Ok(JobLog::default()));
        assert!(lines(&a)[0].text.contains("pas démarré"));
    }

    #[test]
    fn defilement_borne() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        deliver(
            &mut a,
            7,
            Ok(JobLog {
                steps: vec![step("s", Status::Success, 19)],
            }),
        ); // 20 lignes
        a.logs_scroll(-1);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 0);
        a.logs_scroll(100);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 15);
        a.logs_top();
        a.logs_page(1);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 4, "une page = hauteur - 1");
        a.logs_page(-5);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 0);
        a.logs_bottom();
        assert_eq!(a.logs.as_ref().unwrap().scroll, 15);
        // Pas de panique sans vue ni contenu
        App::new().logs_scroll(3);
    }

    #[test]
    fn erreur_de_logs_selon_l_etat() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        deliver(&mut a, 7, Err(Error::SessionExpired));
        assert!(
            matches!(a.logs.as_ref().unwrap().state, LogState::Failed(ref m) if m.contains("reconnexion"))
        );
        // Une relance repasse en chargement, puis un succès remplace l'erreur
        assert!(a.refetch_logs().is_some());
        assert!(matches!(a.logs.as_ref().unwrap().state, LogState::Loading));
        deliver(
            &mut a,
            7,
            Ok(JobLog {
                steps: vec![step("s", Status::Success, 1)],
            }),
        );
        // Échec d'un rafraîchissement : le contenu reste, l'erreur va dans le bandeau
        deliver(&mut a, 7, Err(Error::Network("coupé".into())));
        assert_eq!(lines(&a).len(), 2);
        assert!(a.error.as_deref().unwrap().contains("coupé"));
    }

    #[test]
    fn logs_en_direct_selon_le_statut_courant() {
        let mut a = app_on_run(Status::Running);
        assert!(!a.logs_are_live(), "pas de vue ouverte");
        a.open_logs();
        assert!(a.logs_are_live());
        // Le run passe au vert dans la liste rafraîchie : plus de rafraîchissement
        let mut done = pipe(7);
        done.status = Status::Success;
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![done]),
        });
        assert!(!a.logs_are_live());
    }

    #[test]
    fn fermeture_de_la_vue() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        a.close_logs();
        assert!(a.logs.is_none() && a.refetch_logs().is_none());
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
