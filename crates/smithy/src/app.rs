//! État de l'application : aucune dépendance au terminal, donc testable seul.

use std::cell::Cell;

use smithy_core::{Branch, Entry, Error, FileContent, JobLog, Pipeline, Repo, Status};

use crate::{
    browser::{Browser, Mode, Request},
    logview::{LogState, LogView, Start, TextView},
    session::Session,
};

pub enum Msg {
    /// Explorateur : branches, contenu d'un dossier, contenu d'un fichier. Chaque réponse porte
    /// ce qu'on a demandé, pour que l'explorateur écarte celles qui sont devenues périmées.
    Branches {
        repo: Repo,
        result: Result<Vec<Branch>, Error>,
    },
    Dir {
        repo: Repo,
        reference: String,
        path: String,
        result: Result<Vec<Entry>, Error>,
    },
    File {
        repo: Repo,
        reference: String,
        path: String,
        result: Result<FileContent, Error>,
    },
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
    /// Explorateur de code ouvert sur le dépôt sélectionné, le cas échéant (jamais avec `logs`).
    pub browser: Option<Browser>,
    /// Le backend sait-il parcourir le code ? (`Capabilities::browse`)
    pub browse_supported: bool,
    /// Dernier choix de retour à la ligne, appliqué aux vues de logs suivantes (le temps de la session).
    pub logs_wrap: bool,
    /// Hauteur et largeur utiles de la vue des logs, écrites par le rendu qui seul les connaît.
    pub viewport: Cell<usize>,
    pub viewport_width: Cell<usize>,
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
            browser: None,
            browse_supported: false,
            logs_wrap: false,
            viewport: Cell::new(20),
            viewport_width: Cell::new(80),
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
        let mut view = LogView::new(repo.clone(), pipeline.clone());
        view.wrap = self.logs_wrap;
        self.logs = Some(view);
        Some((repo, pipeline))
    }

    /// Touche `e` : explore le code du dépôt sélectionné.
    pub fn open_browser(&mut self) {
        if !self.browse_supported {
            self.error = Some("ce backend ne permet pas d'explorer le code".to_owned());
            return;
        }
        if let Some(repo) = self.selected_repo().cloned() {
            self.browser = Some(Browser::open(repo));
        }
    }

    pub fn close_browser(&mut self) {
        self.browser = None;
    }

    /// Requêtes que l'explorateur demande de lancer.
    pub fn take_requests(&mut self) -> Vec<Request> {
        self.browser
            .as_mut()
            .map(Browser::take_requests)
            .unwrap_or_default()
    }

    /// Page précédente (`-1`) ou suivante (`1`) de la liste de l'explorateur.
    pub fn browser_page(&mut self, pages: isize) {
        let page = self.viewport.get().saturating_sub(1).max(1) as isize;
        if let Some(b) = self.browser.as_mut() {
            b.move_selection(pages * page);
        }
    }

    /// Le volet de texte à l'écran : la sortie d'un job, ou le fichier ouvert dans l'explorateur.
    fn active_text_mut(&mut self) -> Option<&mut TextView> {
        if let Some(b) = self.browser.as_mut() {
            return match &mut b.mode {
                Mode::File(fv) => Some(&mut fv.text),
                _ => None,
            };
        }
        self.logs.as_deref_mut()
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

    fn view_size(&self) -> (usize, usize) {
        (self.viewport_width.get().max(1), self.viewport.get().max(1))
    }

    pub fn text_scroll(&mut self, delta: isize) {
        let (w, h) = self.view_size();
        if let Some(v) = self.active_text_mut() {
            let max = v.max_scroll(w, h);
            let row = v.scroll.saturating_add_signed(delta);
            v.scroll_to_row(row, max);
        }
    }

    pub fn text_page(&mut self, pages: isize) {
        let page = self.viewport.get().saturating_sub(1).max(1) as isize;
        self.text_scroll(pages * page);
    }

    pub fn text_top(&mut self) {
        if let Some(v) = self.active_text_mut() {
            v.scroll_to_row(0, 0);
        }
    }

    pub fn text_bottom(&mut self) {
        let (w, h) = self.view_size();
        if let Some(v) = self.active_text_mut() {
            let max = v.max_scroll(w, h);
            v.scroll_to_row(max, max);
        }
    }

    /// Espace : plie ou déplie la section (l'étape) en haut de l'écran.
    pub fn text_toggle_section(&mut self) {
        let (w, h) = self.view_size();
        if let Some(v) = self.active_text_mut() {
            v.toggle_section(w, h);
        }
    }

    /// Flèches droite / gauche : section suivante (`1`) ou précédente (`-1`).
    pub fn text_select_section(&mut self, delta: isize) {
        let (w, h) = self.view_size();
        if let Some(v) = self.active_text_mut() {
            v.select_section(delta, w, h);
        }
    }

    /// Touche `a` : tout déplier, ou tout plier si tout est déjà déplié.
    pub fn text_toggle_all(&mut self) {
        let (w, h) = self.view_size();
        if let Some(v) = self.active_text_mut() {
            v.toggle_all(w, h);
        }
    }

    /// Retour à la ligne (touche `w`) : la même ligne de log reste en haut de l'écran.
    pub fn text_toggle_wrap(&mut self) {
        let (w, h) = self.view_size();
        if let Some(v) = self.active_text_mut() {
            v.toggle_wrap(w, h);
            self.logs_wrap = v.wrap;
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
            Msg::Branches { repo, result } => {
                if let Some(b) = self.browser.as_mut() {
                    b.on_branches(&repo, result);
                }
                false
            }
            Msg::Dir {
                repo,
                reference,
                path,
                result,
            } => {
                if let Some(b) = self.browser.as_mut() {
                    b.on_dir(&repo, &reference, &path, result);
                }
                false
            }
            Msg::File {
                repo,
                reference,
                path,
                result,
            } => {
                if let Some(b) = self.browser.as_mut() {
                    b.on_file(&repo, &reference, &path, result);
                }
                false
            }
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
                        let notice = log.notice.clone();
                        let (w, h) = self.view_size();
                        let Some(v) = self.logs.as_mut() else {
                            return false;
                        };
                        let first_load = matches!(v.state, LogState::Loading);
                        let was_at_bottom = v.scroll >= v.max_scroll(w, h);
                        v.notice = notice;
                        v.set_log(&log);
                        let start = first_load.then(|| v.initial_start(live)).flatten();
                        v.scroll = match start {
                            // À l'ouverture : l'étape en échec, ou la fin d'un job en cours.
                            Some(Start::Line(i)) => v.row_of_line(w, i),
                            Some(Start::End) => usize::MAX,
                            None if first_load => 0,
                            // Ensuite : on suit la fin d'un job en cours si on y était.
                            None if live && was_at_bottom => usize::MAX,
                            None => v.scroll,
                        }
                        .min(v.max_scroll(w, h));
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

fn clamp_step(idx: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    idx.saturating_add_signed(delta).min(len - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::Mode;
    use crate::logview::{LineKind, LogLine};
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
            ..Default::default()
        };
        deliver(&mut a, 7, Ok(log));
        // L'étape réussie est repliée (une seule ligne d'écran) : l'étape en échec est la 2e.
        assert_eq!(a.logs.as_ref().unwrap().scroll, 1);
        let all = lines(&a);
        assert_eq!(all[0].text, "▸ Set up job (9 lignes)");
        assert_eq!(all[10].kind, LineKind::Step(Status::Failure));
        assert_eq!(all[10].text, "▾ cargo test");
        assert_eq!(all.len(), 1 + 9 + 1 + 30 + 1 + 2);
    }

    #[test]
    fn job_reussi_ouvert_en_haut_job_en_cours_en_bas() {
        let log = |status| JobLog {
            steps: vec![step("build", status, 20)],
            ..Default::default()
        };
        let mut done = app_on_run(Status::Success);
        done.open_logs();
        deliver(&mut done, 7, Ok(log(Status::Success)));
        assert_eq!(done.logs.as_ref().unwrap().scroll, 0);

        let mut live = app_on_run(Status::Running);
        live.open_logs();
        deliver(&mut live, 7, Ok(log(Status::Running)));
        assert_eq!(live.logs.as_ref().unwrap().scroll, 21 - 5);
    }

    #[test]
    fn job_en_cours_suit_la_fin_sauf_si_on_a_remonte() {
        let mut a = app_on_run(Status::Running);
        a.open_logs();
        let log = |n| JobLog {
            steps: vec![step("build", Status::Running, n)],
            ..Default::default()
        };
        deliver(&mut a, 7, Ok(log(20)));
        deliver(&mut a, 7, Ok(log(30))); // on était en bas : on suit
        assert_eq!(a.logs.as_ref().unwrap().scroll, 31 - 5);
        a.text_scroll(-3);
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
    fn la_reserve_du_backend_est_conservee() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        let log = JobLog {
            steps: vec![step("s", Status::Success, 1)],
            notice: Some("version non vérifiée".into()),
        };
        deliver(&mut a, 7, Ok(log));
        assert_eq!(
            a.logs.as_ref().unwrap().notice.as_deref(),
            Some("version non vérifiée")
        );
        // Un rafraîchissement sans réserve l'efface
        deliver(&mut a, 7, Ok(JobLog::default()));
        assert_eq!(a.logs.as_ref().unwrap().notice, None);
    }

    fn long_log() -> JobLog {
        JobLog {
            steps: vec![smithy_core::StepLog {
                name: "s".into(),
                status: Status::Running, // ouverte : une étape réussie serait repliée
                lines: vec!["x".repeat(25), "fin".into()],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn retour_a_la_ligne_agrandit_le_defilement() {
        let mut a = app_on_run(Status::Success);
        a.viewport.set(3);
        a.viewport_width.set(10);
        a.open_logs();
        deliver(&mut a, 7, Ok(long_log())); // « ▸ s », 25 x, « fin » : 3 lignes
        a.text_bottom();
        assert_eq!(a.logs.as_ref().unwrap().scroll, 0, "tout tient sans repli");

        a.text_toggle_wrap();
        assert!(a.logs.as_ref().unwrap().wrap);
        a.text_bottom(); // 1 + 3 (25 caractères sur 10 colonnes) + 1 = 5 lignes d'écran
        assert_eq!(a.logs.as_ref().unwrap().scroll, 2);
        let v = a.logs.as_ref().unwrap();
        assert_eq!(v.window(10, 3).last().map(|r| r.text), Some("fin"));

        a.text_toggle_wrap();
        assert!(!a.logs.as_ref().unwrap().wrap);
    }

    #[test]
    fn le_retour_a_la_ligne_survit_au_rafraichissement() {
        let mut a = app_on_run(Status::Success);
        a.viewport.set(3);
        a.viewport_width.set(10);
        a.open_logs();
        deliver(&mut a, 7, Ok(long_log()));
        a.text_toggle_wrap();
        deliver(&mut a, 7, Ok(long_log()));
        assert!(a.logs.as_ref().unwrap().wrap);
    }

    #[test]
    fn un_job_en_cours_suit_la_fin_en_mode_replie() {
        let mut a = app_on_run(Status::Running);
        a.viewport.set(3);
        a.viewport_width.set(10);
        a.open_logs();
        deliver(&mut a, 7, Ok(long_log()));
        a.text_toggle_wrap();
        a.text_bottom();
        let more = JobLog {
            steps: vec![smithy_core::StepLog {
                name: "s".into(),
                status: Status::Running,
                lines: vec!["x".repeat(25), "fin".into(), "encore".into()],
            }],
            ..Default::default()
        };
        deliver(&mut a, 7, Ok(more));
        let v = a.logs.as_ref().unwrap();
        assert_eq!(v.window(10, 3).last().map(|r| r.text), Some("encore"));
    }

    #[test]
    fn le_choix_de_repli_est_garde_pour_les_vues_suivantes() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        assert!(!a.logs.as_ref().unwrap().wrap, "désactivé par défaut");
        a.text_toggle_wrap();
        a.close_logs();
        a.open_logs();
        assert!(a.logs.as_ref().unwrap().wrap, "conservé à la réouverture");
        a.text_toggle_wrap();
        a.close_logs();
        a.open_logs();
        assert!(!a.logs.as_ref().unwrap().wrap);
    }

    #[test]
    fn espace_plie_et_deplie_la_section_courante() {
        let mut a = app_on_run(Status::Failure);
        a.viewport.set(3); // 4 lignes d'écran au total : un défilement possible
        a.open_logs();
        let log = JobLog {
            steps: vec![
                step("Set up job", Status::Success, 9),
                step("Tests", Status::Failure, 2),
            ],
            ..Default::default()
        };
        deliver(&mut a, 7, Ok(log));
        let view = |a: &App| -> Vec<String> {
            let v = a.logs.as_ref().unwrap();
            v.window(80, 3).iter().map(|r| r.text.to_owned()).collect()
        };
        // Ouvert sur l'étape en échec (la réussie est repliée au-dessus)
        assert_eq!(a.logs.as_ref().unwrap().scroll, 1);
        assert_eq!(view(&a), ["▾ Tests", "Tests 1", "Tests 2"]);
        a.text_scroll(-1);
        assert_eq!(view(&a)[0], "▸ Set up job (9 lignes)");
        a.text_toggle_section(); // déplier « Set up job »
        assert_eq!(view(&a)[1], "Set up job 1");
        assert_eq!(a.logs.as_ref().unwrap().scroll, 0);
        a.text_toggle_section(); // et la replier
        assert_eq!(view(&a)[0], "▸ Set up job (9 lignes)");
        // `a` : tout déplier (une étape est repliée), puis tout replier
        a.text_toggle_all();
        assert_eq!(a.logs.as_ref().unwrap().window(80, 40).len(), 1 + 9 + 1 + 2);
        a.text_toggle_all();
        assert_eq!(a.logs.as_ref().unwrap().window(80, 40).len(), 2);
        // Flèches : on atteint l'autre section sans défiler (tout tient presque à l'écran)
        a.viewport.set(10); // tout est replié (dernier `a`) et tient à l'écran
        a.text_select_section(1);
        a.text_toggle_section();
        assert_eq!(a.logs.as_ref().unwrap().window(80, 10)[2].text, "Tests 1");
        a.text_select_section(-1);
        a.text_toggle_section();
        assert_eq!(a.logs.as_ref().unwrap().window(80, 40).len(), 1 + 9 + 1 + 2);
        // Aucune vue ouverte : sans effet
        let mut none = App::new();
        none.text_toggle_section();
        none.text_toggle_all();
        none.text_select_section(1);
    }

    // --- explorateur de code

    use smithy_core::{Branch, Entry, EntryKind};

    fn explorer() -> App {
        let mut a = app_on_run(Status::Success);
        a.browse_supported = true;
        a.open_browser();
        a
    }

    fn feed(a: &mut App) {
        let r = repo("a");
        a.apply(Msg::Branches {
            repo: r.clone(),
            result: Ok(vec![Branch {
                name: "main".into(),
                default: true,
            }]),
        });
        a.apply(Msg::Dir {
            repo: r,
            reference: "main".into(),
            path: String::new(),
            result: Ok(vec![
                Entry {
                    name: "src".into(),
                    path: "src".into(),
                    kind: EntryKind::Dir,
                    size: 0,
                },
                Entry {
                    name: "a.txt".into(),
                    path: "a.txt".into(),
                    kind: EntryKind::File,
                    size: 3,
                },
            ]),
        });
    }

    #[test]
    fn ouverture_de_l_explorateur_et_file_de_requetes() {
        let mut a = explorer();
        assert!(a.browser.is_some());
        let reqs = a.take_requests();
        assert_eq!(reqs, [crate::browser::Request::Branches(repo("a"))]);
        assert!(a.take_requests().is_empty(), "la file est vidée");
        feed(&mut a);
        let reqs = a.take_requests();
        assert!(
            matches!(&reqs[..], [crate::browser::Request::Dir { reference, .. }] if reference == "main")
        );
        a.close_browser();
        assert!(a.browser.is_none());
        assert!(
            a.take_requests().is_empty(),
            "sans explorateur, rien à demander"
        );
    }

    #[test]
    fn explorateur_refuse_si_le_backend_ne_sait_pas() {
        let mut a = app_on_run(Status::Success);
        a.open_browser();
        assert!(a.browser.is_none());
        assert!(a.error.is_some());
        let mut none = App::new();
        none.browse_supported = true;
        none.open_browser(); // aucun dépôt sélectionné
        assert!(none.browser.is_none());
    }

    #[test]
    fn les_reponses_sans_explorateur_sont_ignorees() {
        let mut a = app_on_run(Status::Success);
        feed(&mut a);
        a.apply(Msg::File {
            repo: repo("a"),
            reference: "main".into(),
            path: "a.txt".into(),
            result: Err(Error::Network("x".into())),
        });
        assert!(a.browser.is_none());
    }

    #[test]
    fn fichier_de_l_explorateur_defile_et_se_replie() {
        let mut a = explorer();
        feed(&mut a);
        a.viewport.set(3);
        a.viewport_width.set(10);
        {
            let b = a.browser.as_mut().unwrap();
            b.move_selection(1); // « a.txt » (dossiers d'abord)
            b.enter();
        }
        a.apply(Msg::File {
            repo: repo("a"),
            reference: "main".into(),
            path: "a.txt".into(),
            result: Ok(FileContent {
                size: 40,
                text: Some(format!("{}\nfin\n", "x".repeat(25))),
                truncated: false,
            }),
        });
        let rows = |a: &App, w: usize| match &a.browser.as_ref().unwrap().mode {
            Mode::File(fv) => fv.text.total_rows(w),
            _ => 0,
        };
        assert_eq!(rows(&a, 10), 2, "sans repli : 2 lignes");
        a.text_toggle_wrap();
        assert_eq!(
            rows(&a, 10),
            4,
            "25 caractères sur 10 colonnes : 3 lignes d'écran + « fin »"
        );
        assert!(a.logs_wrap, "le choix est partagé avec la vue des logs");
        a.text_bottom();
        let Mode::File(fv) = &a.browser.as_ref().unwrap().mode else {
            panic!()
        };
        assert_eq!(fv.text.scroll, 1);
        // Dans la liste d'un dossier, ces actions sont sans effet
        a.browser.as_mut().unwrap().up();
        a.text_scroll(5);
        a.text_toggle_wrap();
    }

    #[test]
    fn pages_de_l_explorateur() {
        let mut a = explorer();
        feed(&mut a);
        a.viewport.set(1);
        a.browser_page(1);
        assert_eq!(
            a.browser.as_ref().unwrap().selected,
            1,
            "une page = au moins 1 entrée"
        );
        a.browser_page(-1);
        assert_eq!(a.browser.as_ref().unwrap().selected, 0);
        App::new().browser_page(1);
    }

    #[test]
    fn bascule_sans_vue_ouverte() {
        App::new().text_toggle_wrap();
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
                steps: vec![step("s", Status::Failure, 19)],
                ..Default::default()
            }),
        ); // 20 lignes
        a.text_scroll(-1);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 0);
        a.text_scroll(100);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 15);
        a.text_top();
        a.text_page(1);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 4, "une page = hauteur - 1");
        a.text_page(-5);
        assert_eq!(a.logs.as_ref().unwrap().scroll, 0);
        a.text_bottom();
        assert_eq!(a.logs.as_ref().unwrap().scroll, 15);
        // Pas de panique sans vue ni contenu
        App::new().text_scroll(3);
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
                ..Default::default()
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
