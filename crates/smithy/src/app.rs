//! État de l'application : aucune dépendance au terminal, donc testable seul.

use std::cell::Cell;

use smithy_core::{
    Branch, Entry, Error, FileContent, JobLog, Pipeline, Repo, RepoInfo, RepoSource, Status,
};

use crate::{
    browser::{Browser, Mode},
    logview::{LogState, LogView, Start, TextView},
    pages::{HomeRow, HomeView, RepoView, RunsView},
    recent::{self, RecentEntry},
    request::Request,
    session::Session,
    timeago,
};

pub enum Msg {
    /// Accueil : une liste de dépôts, ou la fiche d'un seul. Chaque réponse porte ce qu'on a
    /// demandé, pour écarter celles qui sont devenues périmées.
    Repos {
        source: RepoSource,
        result: Result<Vec<RepoInfo>, Error>,
    },
    RepoInfo {
        repo: Repo,
        result: Result<RepoInfo, Error>,
    },
    Pipelines {
        repo: Repo,
        result: Result<Vec<Pipeline>, String>,
    },
    /// Explorateur : branches, contenu d'un dossier, contenu d'un fichier.
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
}

/// Une page de la pile de navigation, avec son propre état de vue.
#[derive(Debug)]
pub enum Page {
    /// L'accueil : au fond de la pile, toujours.
    Home(HomeView),
    Repo(RepoView),
    Runs(RunsView),
    Logs(LogView),
    Browser(Browser),
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
    /// Message d'erreur ou d'information passager, affiché en pied de page.
    pub error: Option<String>,
    pub should_quit: bool,
    /// Formulaire affiché par-dessus l'interface tant que l'utilisateur n'est pas connecté.
    pub login: Option<LoginForm>,
    /// Nom et URL du serveur, pour le titre du formulaire.
    pub server_label: String,
    /// Pages empilées, comme dans un navigateur : `Échap` dépile et retrouve la page d'avant
    /// telle qu'on l'a laissée. Jamais vide : le fond est toujours l'accueil.
    pub stack: Vec<Page>,
    /// Nom du serveur, pour le fil d'Ariane et l'historique.
    pub server_name: String,
    /// Le backend sait-il fournir des logs ? (`Capabilities::logs`)
    pub logs_supported: bool,
    /// Le backend sait-il parcourir le code ? (`Capabilities::browse`)
    pub browse_supported: bool,
    /// Dernier choix de retour à la ligne, appliqué aux volets de texte suivants (le temps de la session).
    pub logs_wrap: bool,
    /// Hauteur et largeur utiles de la page, écrites par le rendu qui seul les connaît.
    pub viewport: Cell<usize>,
    pub viewport_width: Cell<usize>,
    /// Dépôts ouverts récemment (tous serveurs) ; à sauvegarder quand `recents_dirty`.
    pub recents: Vec<RecentEntry>,
    pub recents_dirty: bool,
    /// Requêtes que la boucle principale doit lancer. Gardées tant qu'aucun fournisseur n'est
    /// prêt (connexion à faire) : elles partent dès que la connexion aboutit.
    pending: Vec<Request>,
}

impl App {
    pub fn new() -> Self {
        let mut home = HomeView::new();
        let pending = home.request_current().into_iter().collect();
        Self {
            error: None,
            should_quit: false,
            login: None,
            server_label: String::new(),
            stack: vec![Page::Home(home)],
            server_name: String::new(),
            logs_supported: false,
            browse_supported: false,
            logs_wrap: false,
            viewport: Cell::new(20),
            viewport_width: Cell::new(80),
            recents: Vec::new(),
            recents_dirty: false,
            pending,
        }
    }

    // --- la page affichée, selon son type

    pub fn home(&self) -> Option<&HomeView> {
        match self.stack.last() {
            Some(Page::Home(h)) => Some(h),
            _ => None,
        }
    }

    pub fn home_mut(&mut self) -> Option<&mut HomeView> {
        match self.stack.last_mut() {
            Some(Page::Home(h)) => Some(h),
            _ => None,
        }
    }

    pub fn repo_page(&self) -> Option<&RepoView> {
        match self.stack.last() {
            Some(Page::Repo(r)) => Some(r),
            _ => None,
        }
    }

    /// Pour les tests du rendu : modifie la fiche de la page du dépôt affichée.
    #[cfg(test)]
    pub fn repo_page_info_mut(&mut self, f: impl FnOnce(&mut RepoInfo)) {
        if let Some(Page::Repo(r)) = self.stack.last_mut() {
            f(&mut r.info);
        }
    }

    pub fn runs(&self) -> Option<&RunsView> {
        match self.stack.last() {
            Some(Page::Runs(r)) => Some(r),
            _ => None,
        }
    }

    /// Vue des logs, si c'est la page affichée.
    pub fn logs(&self) -> Option<&LogView> {
        match self.stack.last() {
            Some(Page::Logs(v)) => Some(v),
            _ => None,
        }
    }

    pub fn logs_mut(&mut self) -> Option<&mut LogView> {
        match self.stack.last_mut() {
            Some(Page::Logs(v)) => Some(v),
            _ => None,
        }
    }

    /// Explorateur de code, si c'est la page affichée.
    pub fn browser(&self) -> Option<&Browser> {
        match self.stack.last() {
            Some(Page::Browser(b)) => Some(b),
            _ => None,
        }
    }

    pub fn browser_mut(&mut self) -> Option<&mut Browser> {
        match self.stack.last_mut() {
            Some(Page::Browser(b)) => Some(b),
            _ => None,
        }
    }

    /// Où l'on est, du serveur à la page affichée : chaque page ajoute son segment.
    /// `maison › Accueil › smithy/demo › CI › fail.yml › test`.
    pub fn breadcrumb(&self) -> Vec<String> {
        let server = if self.server_name.is_empty() {
            "smithy"
        } else {
            &self.server_name
        };
        let mut out = vec![server.to_owned()];
        for page in &self.stack {
            match page {
                Page::Home(_) => out.push("Accueil".to_owned()),
                Page::Repo(r) => out.push(r.info.repo.full_name()),
                Page::Runs(_) => out.push("CI".to_owned()),
                Page::Logs(v) => out.extend([v.pipeline.workflow.clone(), v.pipeline.job.clone()]),
                Page::Browser(b) => out.extend(b.breadcrumb()),
            }
        }
        out
    }

    /// Dépile la page affichée ; l'accueil ne se dépile jamais. Renvoie `true` si une page est partie.
    pub fn pop(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            true
        } else {
            false
        }
    }

    /// Les requêtes à lancer : celles de l'application, puis celles de chaque explorateur ouvert.
    pub fn take_requests(&mut self) -> Vec<Request> {
        let mut out = std::mem::take(&mut self.pending);
        for page in &mut self.stack {
            if let Page::Browser(b) = page {
                out.extend(b.take_requests());
            }
        }
        out
    }

    // --- accueil

    /// Les lignes de la section affichée (filtre appliqué).
    pub fn home_rows(&self) -> Vec<HomeRow> {
        self.home()
            .map(|h| h.rows(&self.recents, &self.server_name))
            .unwrap_or_default()
    }

    /// Haut, bas, page : déplace la sélection de la liste affichée (accueil, runs ou dossier).
    pub fn list_move(&mut self, delta: isize) {
        let len = self.home_rows().len();
        match self.stack.last_mut() {
            Some(Page::Home(h)) => h.move_selection(delta, len),
            Some(Page::Repo(r)) => r.move_selection(delta),
            Some(Page::Runs(r)) => r.move_selection(delta),
            Some(Page::Browser(b)) => b.move_selection(delta),
            _ => {}
        }
    }

    /// Une page (`1`) ou la précédente (`-1`) : la hauteur affichée moins une ligne.
    pub fn list_page(&mut self, pages: isize) {
        let page = self.viewport.get().saturating_sub(1).max(1) as isize;
        self.list_move(pages * page);
    }

    /// Flèche droite / gauche : section suivante ou précédente de l'accueil.
    pub fn home_switch(&mut self, delta: isize) {
        if let Some(r) = self.home_mut().and_then(|h| h.switch(delta)) {
            self.pending.push(r);
        }
    }

    /// Entrée dans la saisie de l'accueil : « Explorer » lance sa recherche.
    pub fn home_commit(&mut self) {
        if let Some(r) = self.home_mut().and_then(HomeView::commit) {
            self.pending.push(r);
        }
    }

    /// Entrée sur un dépôt de l'accueil : ouvre sa page.
    pub fn home_enter(&mut self) {
        let row = {
            let Some(h) = self.home() else { return };
            let rows = self.home_rows();
            rows.get(h.selected(rows.len())).cloned()
        };
        let Some(row) = row else { return };
        let view = match row.info {
            Some(info) => RepoView::new(info),
            None => RepoView::placeholder(row.repo.clone()),
        };
        self.record_recent(&row.repo);
        self.pending.push(Request::RepoInfo(row.repo.clone()));
        self.pending.push(Request::Pipelines(row.repo));
        self.stack.push(Page::Repo(view));
    }

    fn record_recent(&mut self, repo: &Repo) {
        recent::push(
            &mut self.recents,
            RecentEntry {
                server: self.server_name.clone(),
                owner: repo.owner.clone(),
                name: repo.name.clone(),
                opened: timeago::now_epoch(),
            },
        );
        self.recents_dirty = true;
    }

    // --- page d'un dépôt

    /// Entrée sur une rubrique du dépôt : la CI ou le code.
    pub fn repo_enter(&mut self) {
        let Some(r) = self.repo_page() else { return };
        let (repo, item) = (r.info.repo.clone(), r.selected);
        if item == 0 {
            self.pending.push(Request::Pipelines(repo.clone()));
            self.stack.push(Page::Runs(RunsView::new(repo)));
        } else if self.browse_supported {
            self.stack.push(Page::Browser(Browser::open(repo)));
        } else {
            self.error = Some("ce backend ne permet pas d'explorer le code".to_owned());
        }
    }

    // --- runs et logs

    /// Entrée sur un run : ouvre ses logs.
    pub fn open_logs(&mut self) {
        if !self.logs_supported {
            self.error = Some("ce backend ne fournit pas les logs".to_owned());
            return;
        }
        let Some(runs) = self.runs() else { return };
        let Some(pipeline) = runs.selected_pipeline().cloned() else {
            return;
        };
        let repo = runs.repo.clone();
        let mut view = LogView::new(repo.clone(), pipeline.clone());
        view.wrap = self.logs_wrap;
        self.stack.push(Page::Logs(view));
        self.pending.push(Request::Logs { repo, pipeline });
    }

    /// Redemande les logs affichés. Le contenu déjà là reste visible pendant ce temps ;
    /// une erreur précédente repasse en chargement.
    pub fn refetch_logs(&mut self) {
        let Some(v) = self.logs_mut() else { return };
        if matches!(v.state, LogState::Failed(_)) {
            v.state = LogState::Loading;
        }
        let req = Request::Logs {
            repo: v.repo.clone(),
            pipeline: v.pipeline.clone(),
        };
        self.pending.push(req);
    }

    /// Le job affiché tourne encore : ses logs sont à rafraîchir périodiquement. Son statut
    /// courant est celui de la liste des runs, plus bas dans la pile, qui se rafraîchit aussi.
    pub fn logs_are_live(&self) -> bool {
        let Some(v) = self.logs() else { return false };
        let current = self
            .stack
            .iter()
            .find_map(|p| match p {
                Page::Runs(r) if r.repo == v.repo => {
                    r.pipelines.iter().find(|x| x.id == v.pipeline.id)
                }
                _ => None,
            })
            .map_or(v.pipeline.status, |p| p.status);
        matches!(current, Status::Running | Status::Pending)
    }

    /// Toutes les 5 s : ce qui change tout seul sur la page affichée (les runs d'un dépôt, les
    /// logs d'un job en cours).
    pub fn tick(&mut self) {
        match self.stack.last() {
            Some(Page::Runs(r)) => {
                let repo = r.repo.clone();
                self.pending.push(Request::Pipelines(repo));
            }
            Some(Page::Logs(v)) => {
                // La liste des runs, plus bas, dit si le job a fini.
                let repo = v.repo.clone();
                self.pending.push(Request::Pipelines(repo));
                if self.logs_are_live() {
                    self.refetch_logs();
                }
            }
            _ => {}
        }
    }

    /// Touche `r` : recharge la page affichée.
    pub fn refresh_current(&mut self) {
        let mut requests = Vec::new();
        match self.stack.last_mut() {
            Some(Page::Home(h)) => requests.extend(h.refresh()),
            Some(Page::Repo(r)) => {
                let repo = r.info.repo.clone();
                requests.push(Request::RepoInfo(repo.clone()));
                requests.push(Request::Pipelines(repo));
            }
            Some(Page::Runs(r)) => requests.push(Request::Pipelines(r.repo.clone())),
            Some(Page::Browser(b)) => b.refresh(),
            Some(Page::Logs(_)) => self.refetch_logs(),
            None => {}
        }
        self.pending.extend(requests);
    }

    pub fn close_browser(&mut self) {
        if self.browser().is_some() {
            self.pop();
        }
    }

    pub fn close_logs(&mut self) {
        if self.logs().is_some() {
            self.pop();
        }
    }

    /// Le volet de texte à l'écran : la sortie d'un job, ou le fichier ouvert dans l'explorateur.
    fn active_text_mut(&mut self) -> Option<&mut TextView> {
        match self.stack.last_mut()? {
            Page::Browser(b) => match &mut b.mode {
                Mode::File(fv) => Some(&mut fv.text),
                _ => None, // la liste d'un dossier n'est pas un volet de texte
            },
            Page::Logs(v) => Some(&mut **v),
            _ => None,
        }
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

    /// Intègre une réponse du réseau. Les réponses périmées (page dépilée, autre dépôt, autre
    /// recherche) sont ignorées par les pages concernées.
    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Repos { source, result } => {
                if let Some(Page::Home(h)) = self.stack.first_mut() {
                    h.apply(&source, result);
                }
            }
            Msg::RepoInfo { repo, result } => {
                for page in &mut self.stack {
                    if let Page::Repo(r) = page {
                        let result = match &result {
                            Ok(i) => Ok(i.clone()),
                            Err(e) => Err(Error::Network(e.to_string())),
                        };
                        r.apply_info(&repo, result);
                    }
                }
            }
            Msg::Pipelines { repo, result } => {
                for page in &mut self.stack {
                    match page {
                        Page::Repo(r) => r.apply_pipelines(&repo, &result),
                        Page::Runs(r) => r.apply_pipelines(&repo, result.clone()),
                        _ => {}
                    }
                }
            }
            Msg::Branches { repo, result } => {
                if let Some(b) = self.browser_mut() {
                    b.on_branches(&repo, result);
                }
            }
            Msg::Dir {
                repo,
                reference,
                path,
                result,
            } => {
                if let Some(b) = self.browser_mut() {
                    b.on_dir(&repo, &reference, &path, result);
                }
            }
            Msg::File {
                repo,
                reference,
                path,
                result,
            } => {
                if let Some(b) = self.browser_mut() {
                    b.on_file(&repo, &reference, &path, result);
                }
            }
            Msg::Logs {
                repo,
                pipeline_id,
                result,
            } => {
                // Réponse périmée : la vue a été fermée ou ouverte sur un autre job.
                let current = self
                    .logs()
                    .is_some_and(|v| v.repo == repo && v.pipeline.id == pipeline_id);
                if !current {
                    return;
                }
                match result {
                    Ok(log) => {
                        let live = self.logs_are_live();
                        let notice = log.notice.clone();
                        let (w, h) = self.view_size();
                        let Some(v) = self.logs_mut() else {
                            return;
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
                            .logs()
                            .is_some_and(|v| matches!(v.state, LogState::Ready(_)));
                        if ready {
                            // On garde ce qu'on affiche, l'échec va dans le bandeau.
                            self.error = Some(e.to_string());
                        } else if let Some(v) = self.logs_mut() {
                            v.state = LogState::Failed(e.to_string());
                        }
                    }
                }
            }
            Msg::LoggedIn(Ok(_)) => {
                self.login = None;
                self.error = None;
            }
            Msg::LoggedIn(Err(e)) => {
                if let Some(f) = self.login.as_mut() {
                    f.busy = false;
                    f.field = LoginField::Password;
                    f.error = Some(e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::Mode;
    use crate::logview::{LineKind, LogLine};
    use crate::pages::LastRun;
    use smithy_core::{Branch, Entry, EntryKind, Status};

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

    fn deliver(a: &mut App, id: u64, result: Result<JobLog, Error>) {
        a.apply(Msg::Logs {
            repo: repo("a"),
            pipeline_id: id,
            result,
        });
    }

    fn lines(a: &App) -> &Vec<LogLine> {
        match &a.logs().unwrap().state {
            LogState::Ready(l) => l,
            s => panic!("pas prêt : {s:?}"),
        }
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

    fn typed(app: &mut App, text: &str) {
        text.chars().for_each(|c| app.login_type(c));
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

    fn info(n: &str) -> RepoInfo {
        RepoView::placeholder(repo(n)).info
    }

    /// App au repos : le démarrage a déjà demandé « mes dépôts », la file est vidée.
    fn fresh() -> App {
        let mut a = App::new();
        a.logs_supported = true;
        a.browse_supported = true;
        a.take_requests();
        a
    }

    /// App sur la page d'un dépôt (`o/a`), prête à ouvrir la CI ou le code.
    fn app_on_repo() -> App {
        let mut a = fresh();
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info("a"), info("b")]),
        });
        a.home_enter();
        a.take_requests();
        a
    }

    /// App sur la page des runs de `o/a`, un run (id 7) sélectionné, logs et explorateur pris en charge.
    fn app_on_run(status: Status) -> App {
        let mut a = app_on_repo();
        a.repo_enter();
        let mut p = pipe(7);
        p.status = status;
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![p]),
        });
        a.viewport.set(5);
        a.take_requests();
        a
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
        assert_eq!(a.logs().unwrap().scroll, 1);
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
        assert_eq!(done.logs().unwrap().scroll, 0);

        let mut live = app_on_run(Status::Running);
        live.open_logs();
        deliver(&mut live, 7, Ok(log(Status::Running)));
        assert_eq!(live.logs().unwrap().scroll, 21 - 5);
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
        assert_eq!(a.logs().unwrap().scroll, 31 - 5);
        a.text_scroll(-3);
        let pos = a.logs().unwrap().scroll;
        deliver(&mut a, 7, Ok(log(40))); // on a remonté : on reste
        assert_eq!(a.logs().unwrap().scroll, pos);
    }

    #[test]
    fn reponse_de_logs_perimee_ignoree() {
        let mut a = app_on_run(Status::Success);
        deliver(&mut a, 7, Ok(JobLog::default())); // vue fermée
        assert!(a.logs().is_none());
        a.open_logs();
        deliver(&mut a, 8, Ok(JobLog::default())); // autre run
        assert!(matches!(a.logs().unwrap().state, LogState::Loading));
        a.apply(Msg::Logs {
            repo: repo("autre"),
            pipeline_id: 7,
            result: Ok(JobLog::default()),
        });
        assert!(matches!(a.logs().unwrap().state, LogState::Loading));
    }

    #[test]
    fn ouverture_des_logs() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        let v = a.logs().unwrap();
        assert_eq!((v.repo.clone(), v.pipeline.id), (repo("a"), 7));
        assert!(matches!(v.state, LogState::Loading));
        assert!(
            matches!(&a.take_requests()[..], [Request::Logs { pipeline, .. }] if pipeline.id == 7)
        );

        let mut b = app_on_run(Status::Success);
        b.logs_supported = false;
        b.open_logs();
        assert!(b.logs().is_none() && b.error.is_some());
        assert!(b.take_requests().is_empty());

        // Aucun run (liste vide) ou pas sur la page des runs : rien ne s'ouvre
        let mut c = app_on_repo();
        c.repo_enter();
        c.open_logs();
        assert!(c.logs().is_none());
        let mut d = app_on_repo();
        d.open_logs();
        assert!(d.logs().is_none() && d.repo_page().is_some());
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
            a.logs().unwrap().notice.as_deref(),
            Some("version non vérifiée")
        );
        // Un rafraîchissement sans réserve l'efface
        deliver(&mut a, 7, Ok(JobLog::default()));
        assert_eq!(a.logs().unwrap().notice, None);
    }

    #[test]
    fn retour_a_la_ligne_agrandit_le_defilement() {
        let mut a = app_on_run(Status::Success);
        a.viewport.set(3);
        a.viewport_width.set(10);
        a.open_logs();
        deliver(&mut a, 7, Ok(long_log())); // « ▸ s », 25 x, « fin » : 3 lignes
        a.text_bottom();
        assert_eq!(a.logs().unwrap().scroll, 0, "tout tient sans repli");

        a.text_toggle_wrap();
        assert!(a.logs().unwrap().wrap);
        a.text_bottom(); // 1 + 3 (25 caractères sur 10 colonnes) + 1 = 5 lignes d'écran
        assert_eq!(a.logs().unwrap().scroll, 2);
        let v = a.logs().unwrap();
        assert_eq!(v.window(10, 3).last().map(|r| r.text), Some("fin"));

        a.text_toggle_wrap();
        assert!(!a.logs().unwrap().wrap);
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
        assert!(a.logs().unwrap().wrap);
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
        let v = a.logs().unwrap();
        assert_eq!(v.window(10, 3).last().map(|r| r.text), Some("encore"));
    }

    #[test]
    fn le_choix_de_repli_est_garde_pour_les_vues_suivantes() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        assert!(!a.logs().unwrap().wrap, "désactivé par défaut");
        a.text_toggle_wrap();
        a.close_logs();
        a.open_logs();
        assert!(a.logs().unwrap().wrap, "conservé à la réouverture");
        a.text_toggle_wrap();
        a.close_logs();
        a.open_logs();
        assert!(!a.logs().unwrap().wrap);
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
            let v = a.logs().unwrap();
            v.window(80, 3).iter().map(|r| r.text.to_owned()).collect()
        };
        // Ouvert sur l'étape en échec (la réussie est repliée au-dessus)
        assert_eq!(a.logs().unwrap().scroll, 1);
        assert_eq!(view(&a), ["▾ Tests", "Tests 1", "Tests 2"]);
        a.text_scroll(-1);
        assert_eq!(view(&a)[0], "▸ Set up job (9 lignes)");
        a.text_toggle_section(); // déplier « Set up job »
        assert_eq!(view(&a)[1], "Set up job 1");
        assert_eq!(a.logs().unwrap().scroll, 0);
        a.text_toggle_section(); // et la replier
        assert_eq!(view(&a)[0], "▸ Set up job (9 lignes)");
        // `a` : tout déplier (une étape est repliée), puis tout replier
        a.text_toggle_all();
        assert_eq!(a.logs().unwrap().window(80, 40).len(), 1 + 9 + 1 + 2);
        a.text_toggle_all();
        assert_eq!(a.logs().unwrap().window(80, 40).len(), 2);
        // Flèches : on atteint l'autre section sans défiler (tout tient presque à l'écran)
        a.viewport.set(10); // tout est replié (dernier `a`) et tient à l'écran
        a.text_select_section(1);
        a.text_toggle_section();
        assert_eq!(a.logs().unwrap().window(80, 10)[2].text, "Tests 1");
        a.text_select_section(-1);
        a.text_toggle_section();
        assert_eq!(a.logs().unwrap().window(80, 40).len(), 1 + 9 + 1 + 2);
        // Aucune vue ouverte : sans effet
        let mut none = App::new();
        none.text_toggle_section();
        none.text_toggle_all();
        none.text_select_section(1);
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
        assert_eq!(a.logs().unwrap().scroll, 0);
        a.text_scroll(100);
        assert_eq!(a.logs().unwrap().scroll, 15);
        a.text_top();
        a.text_page(1);
        assert_eq!(a.logs().unwrap().scroll, 4, "une page = hauteur - 1");
        a.text_page(-5);
        assert_eq!(a.logs().unwrap().scroll, 0);
        a.text_bottom();
        assert_eq!(a.logs().unwrap().scroll, 15);
        // Pas de panique sans vue ni contenu
        App::new().text_scroll(3);
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
    fn les_reponses_sans_explorateur_sont_ignorees() {
        let mut a = app_on_run(Status::Success);
        feed(&mut a);
        a.apply(Msg::File {
            repo: repo("a"),
            reference: "main".into(),
            path: "a.txt".into(),
            result: Err(Error::Network("x".into())),
        });
        assert!(a.browser().is_none());
    }

    #[test]
    fn fichier_de_l_explorateur_defile_et_se_replie() {
        let mut a = explorer();
        feed(&mut a);
        a.viewport.set(3);
        a.viewport_width.set(10);
        {
            let b = a.browser_mut().unwrap();
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
        let rows = |a: &App, w: usize| match &a.browser().unwrap().mode {
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
        let Mode::File(fv) = &a.browser().unwrap().mode else {
            panic!()
        };
        assert_eq!(fv.text.scroll, 1);
        // Dans la liste d'un dossier, ces actions sont sans effet
        a.browser_mut().unwrap().up();
        a.text_scroll(5);
        a.text_toggle_wrap();
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
    fn erreur_de_logs_selon_l_etat() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        a.take_requests();
        deliver(&mut a, 7, Err(Error::SessionExpired));
        assert!(
            matches!(a.logs().unwrap().state, LogState::Failed(ref m) if m.contains("reconnexion"))
        );
        // Une relance repasse en chargement, puis un succès remplace l'erreur
        a.refetch_logs();
        assert!(matches!(&a.take_requests()[..], [Request::Logs { .. }]));
        assert!(matches!(a.logs().unwrap().state, LogState::Loading));
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
    fn fermeture_de_la_vue() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        a.take_requests();
        a.close_logs();
        assert!(a.logs().is_none() && a.runs().is_some());
        a.refetch_logs(); // plus de vue : rien à redemander
        assert!(a.take_requests().is_empty());
    }

    // --- démarrage, accueil, page d'un dépôt

    #[test]
    fn demarrage_sur_l_accueil_qui_demande_mes_depots() {
        let mut a = App::new();
        assert!(matches!(a.stack[..], [Page::Home(_)]));
        assert_eq!(a.take_requests(), [Request::Repos(RepoSource::Mine)]);
        assert!(a.take_requests().is_empty(), "la file est vidée");
        assert_eq!(a.breadcrumb(), ["smithy", "Accueil"]);
    }

    #[test]
    fn les_requetes_attendent_d_etre_prises() {
        // Sans fournisseur (connexion à faire), la boucle ne les prend pas : elles restent.
        let mut a = App::new();
        a.home_switch(1);
        a.home_switch(-1);
        let first = a.take_requests();
        assert_eq!(
            first,
            [
                Request::Repos(RepoSource::Mine),
                Request::Repos(RepoSource::Watched)
            ]
        );
    }

    #[test]
    fn ouvrir_un_depot_depuis_l_accueil() {
        let mut a = fresh();
        a.server_name = "maison".into();
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info("a"), info("b")]),
        });
        a.list_move(1); // « b »
        a.home_enter();
        let r = a.repo_page().expect("page du dépôt");
        assert_eq!(r.info.repo, repo("b"));
        assert_eq!(
            a.take_requests(),
            [Request::RepoInfo(repo("b")), Request::Pipelines(repo("b"))]
        );
        assert_eq!(a.breadcrumb(), ["maison", "Accueil", "o/b"]);
        // L'historique retient le dépôt, une seule fois, le plus récent en tête
        assert!(a.recents_dirty);
        assert_eq!(
            (a.recents[0].server.as_str(), a.recents[0].name.as_str()),
            ("maison", "b")
        );
        a.pop();
        a.list_move(-1);
        a.home_enter();
        a.pop();
        a.list_move(1);
        a.home_enter();
        assert_eq!(
            a.recents
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[test]
    fn rien_ne_s_ouvre_sur_une_liste_vide() {
        let mut a = fresh();
        a.home_enter(); // « Mes dépôts » encore en chargement
        assert!(a.repo_page().is_none());
        assert!(a.take_requests().is_empty());
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Err(Error::Network("coupé".into())),
        });
        a.home_enter();
        assert!(a.repo_page().is_none());
    }

    #[test]
    fn ouvrir_depuis_les_recents_sans_fiche() {
        let mut a = fresh();
        a.server_name = "s".into();
        a.recents = vec![RecentEntry {
            server: "s".into(),
            owner: "o".into(),
            name: "vieux".into(),
            opened: 5,
        }];
        a.home_switch(4); // « Récents »
        assert!(a.take_requests().is_empty(), "local : rien à charger");
        assert_eq!(a.home_rows().len(), 1);
        a.home_enter();
        let r = a.repo_page().unwrap();
        assert!(
            r.info.default_branch.is_empty(),
            "fiche vide en attendant le serveur"
        );
        a.apply(Msg::RepoInfo {
            repo: repo("vieux"),
            result: Ok(info("vieux")),
        });
        assert_eq!(a.repo_page().unwrap().info.repo.name, "vieux");
        // La fiche d'un autre dépôt est ignorée
        let mut other = info("autre");
        other.description = "x".into();
        a.apply(Msg::RepoInfo {
            repo: repo("autre"),
            result: Ok(other),
        });
        assert_eq!(a.repo_page().unwrap().info.description, "");
    }

    #[test]
    fn de_la_page_du_depot_vers_la_ci_et_le_code() {
        let mut a = app_on_repo();
        a.repo_enter(); // « CI » par défaut
        assert!(a.runs().is_some());
        assert_eq!(a.take_requests(), [Request::Pipelines(repo("a"))]);
        assert_eq!(a.breadcrumb(), ["smithy", "Accueil", "o/a", "CI"]);
        a.pop();
        a.list_move(1); // « Code »
        a.repo_enter();
        assert!(a.browser().is_some());
        assert_eq!(a.take_requests(), [Request::Branches(repo("a"))]);
        assert_eq!(a.breadcrumb(), ["smithy", "Accueil", "o/a", "Code", "…"]);
    }

    #[test]
    fn explorateur_refuse_si_le_backend_ne_sait_pas() {
        let mut a = app_on_repo();
        a.browse_supported = false;
        a.list_move(1);
        a.repo_enter();
        assert!(a.browser().is_none() && a.repo_page().is_some());
        assert!(a.error.as_deref().unwrap().contains("explorer"));
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn les_reponses_de_runs_atteignent_toute_la_pile() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        a.take_requests();
        // La liste arrive alors que les logs sont affichés : la page du dépôt, plus bas, la reçoit
        let mut newer = pipe(9);
        newer.status = Status::Failure;
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![newer, pipe(7)]),
        });
        a.pop();
        assert_eq!(a.runs().unwrap().pipelines.len(), 2);
        a.pop();
        let LastRun::Ready(p) = &a.repo_page().unwrap().last_run else {
            panic!("dernier run attendu")
        };
        assert_eq!((p.id, p.status), (9, Status::Failure));
        // Un autre dépôt est ignoré partout
        a.apply(Msg::Pipelines {
            repo: repo("zzz"),
            result: Ok(vec![]),
        });
        assert!(matches!(a.repo_page().unwrap().last_run, LastRun::Ready(_)));
    }

    #[test]
    fn rafraichissement_et_tick_selon_la_page() {
        // Accueil : `r` recharge la section ; le tick ne fait rien
        let mut a = fresh();
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info("a")]),
        });
        a.tick();
        assert!(a.take_requests().is_empty());
        a.refresh_current();
        assert_eq!(a.take_requests(), [Request::Repos(RepoSource::Mine)]);
        // Page du dépôt : la fiche et la CI ; le tick ne fait rien
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info("a")]),
        });
        a.home_enter();
        a.take_requests();
        a.tick();
        assert!(a.take_requests().is_empty());
        a.refresh_current();
        assert_eq!(
            a.take_requests(),
            [Request::RepoInfo(repo("a")), Request::Pipelines(repo("a"))]
        );
        // Runs : le tick renouvelle la liste
        a.repo_enter();
        a.take_requests();
        a.tick();
        assert_eq!(a.take_requests(), [Request::Pipelines(repo("a"))]);
        a.refresh_current();
        assert_eq!(a.take_requests(), [Request::Pipelines(repo("a"))]);
    }

    #[test]
    fn tick_dans_les_logs_suit_les_runs_et_les_jobs_en_cours() {
        let mut done = app_on_run(Status::Success);
        done.open_logs();
        done.take_requests();
        done.tick();
        assert_eq!(
            done.take_requests(),
            [Request::Pipelines(repo("a"))],
            "job fini : seulement les runs"
        );

        let mut live = app_on_run(Status::Running);
        live.open_logs();
        live.take_requests();
        live.tick();
        let reqs = live.take_requests();
        assert!(reqs.contains(&Request::Pipelines(repo("a"))));
        assert!(
            reqs.iter().any(|r| matches!(r, Request::Logs { .. })),
            "job en cours : ses logs aussi"
        );
        // Le job se termine : la liste des runs, sous les logs, le dit
        live.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![pipe(7)]),
        });
        live.tick();
        assert!(
            !live
                .take_requests()
                .iter()
                .any(|r| matches!(r, Request::Logs { .. }))
        );
    }

    #[test]
    fn deplacements_dans_les_listes() {
        let mut a = fresh();
        let many: Vec<_> = (0..30).map(|i| info(&format!("r{i:02}"))).collect();
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(many),
        });
        a.viewport.set(11);
        a.list_page(1);
        assert_eq!(a.home().unwrap().selected(30), 10, "une page = hauteur - 1");
        a.list_move(isize::MAX / 2);
        assert_eq!(a.home().unwrap().selected(30), 29);
        a.list_move(isize::MIN / 2);
        assert_eq!(a.home().unwrap().selected(30), 0);
        // Sans effet (et sans panique) sur les pages de texte
        let mut l = app_on_run(Status::Success);
        l.open_logs();
        l.list_move(3);
        l.list_page(1);
        // Page du dépôt : deux rubriques
        let mut r = app_on_repo();
        r.list_move(5);
        assert_eq!(r.repo_page().unwrap().selected, 1);
        // Runs
        let mut u = app_on_run(Status::Success);
        u.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok((1..=20).map(pipe).collect()),
        });
        // La sélection suit « son » run (id 7, maintenant le 7e) ; viewport 5 : une page = 4 lignes.
        assert_eq!(u.runs().unwrap().selected, 6);
        u.list_page(1);
        assert_eq!(u.runs().unwrap().selected, 10);
        // Explorateur
        let mut e = explorer();
        feed(&mut e);
        e.viewport.set(1);
        e.list_page(1);
        assert_eq!(
            e.browser().unwrap().selected,
            1,
            "une page = au moins 1 entrée"
        );
        e.list_page(-1);
        assert_eq!(e.browser().unwrap().selected, 0);
        App::new().list_page(1);
    }

    #[test]
    fn recherche_et_commit_depuis_l_accueil() {
        let mut a = fresh();
        a.home_switch(3); // « Explorer »
        a.home_mut().unwrap().start_edit();
        "tree"
            .chars()
            .for_each(|c| a.home_mut().unwrap().type_char(c));
        a.home_commit();
        assert_eq!(
            a.take_requests(),
            [Request::Repos(RepoSource::Search("tree".into()))]
        );
        a.apply(Msg::Repos {
            source: RepoSource::Search("tree".into()),
            result: Ok(vec![info("tree")]),
        });
        assert_eq!(a.home_rows().len(), 1);
        a.home_enter();
        assert_eq!(a.repo_page().unwrap().info.repo.name, "tree");
        // Un message pour une page qui n'est plus l'accueil n'est pas perdu : il met la liste à jour
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info("a")]),
        });
        a.pop();
        a.home_switch(-3);
        assert_eq!(a.home_rows()[0].repo.name, "a");
    }

    // --- pile de pages

    #[test]
    fn la_pile_ne_se_vide_jamais() {
        let mut a = app_on_run(Status::Success);
        assert_eq!(a.stack.len(), 3, "accueil, dépôt, runs");
        a.open_logs();
        assert_eq!(a.stack.len(), 4);
        assert!(a.pop() && a.pop() && a.pop());
        assert!(!a.pop(), "l'accueil ne se dépile pas");
        assert!(matches!(a.stack[..], [Page::Home(_)]));
        assert!(a.logs().is_none() && a.browser().is_none() && a.runs().is_none());
    }

    #[test]
    fn fermer_une_page_ne_depile_que_la_bonne() {
        let mut a = app_on_run(Status::Success);
        a.open_logs();
        a.close_browser(); // l'explorateur n'est pas la page affichée : rien ne bouge
        assert!(a.logs().is_some());
        a.close_logs();
        assert!(a.runs().is_some());
        a.close_logs(); // les logs ne sont plus affichés : sans effet
        a.close_browser();
        assert_eq!(a.stack.len(), 3);
        a.pop();
        a.list_move(1);
        a.repo_enter();
        assert!(a.browser().is_some() && a.logs().is_none());
        a.close_logs(); // les logs ne sont pas la page affichée
        assert!(a.browser().is_some());
        a.close_browser();
        assert!(a.repo_page().is_some());
    }

    #[test]
    fn fil_d_ariane_de_la_pile() {
        let mut a = app_on_run(Status::Success);
        a.server_name = "maison".into();
        assert_eq!(a.breadcrumb(), ["maison", "Accueil", "o/a", "CI"]);
        a.open_logs();
        assert_eq!(
            a.breadcrumb(),
            ["maison", "Accueil", "o/a", "CI", "ci.yml", "build"]
        );
        let anonymous = App::new();
        assert_eq!(anonymous.breadcrumb()[0], "smithy");
    }

    // --- explorateur de code

    fn explorer() -> App {
        let mut a = app_on_repo();
        a.list_move(1); // « Code »
        a.repo_enter();
        a
    }

    #[test]
    fn ouverture_de_l_explorateur_et_file_de_requetes() {
        let mut a = app_on_repo();
        a.list_move(1);
        a.repo_enter();
        assert!(a.browser().is_some());
        assert_eq!(a.take_requests(), [Request::Branches(repo("a"))]);
        assert!(a.take_requests().is_empty(), "la file est vidée");
        feed(&mut a);
        let reqs = a.take_requests();
        assert!(matches!(&reqs[..], [Request::Dir { reference, .. }] if reference == "main"));
        a.close_browser();
        assert!(a.browser().is_none());
        assert!(
            a.take_requests().is_empty(),
            "sans explorateur, rien à demander"
        );
    }
}
