//! État de l'application : aucune dépendance au terminal, donc testable seul.

use std::cell::Cell;

use smithy_core::{
    Branch, Entry, Error, FileContent, JobLog, Pipeline, Repo, RepoInfo, RepoSource, Status,
};

use crate::{
    browser::{Browser, Mode},
    highlight::Run,
    logview::{LogState, LogView, Start, TextView},
    pages::{HomeRow, HomeView, RepoView, RunsView},
    recent::{self, RecentEntry},
    request::Request,
    session::Session,
    timeago,
    workspace::{SavedPage, SavedTab, SavedTabs},
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
    /// La coloration syntaxique d'un fichier, calculée à part une fois le texte affiché.
    FileStyles {
        repo: Repo,
        reference: String,
        path: String,
        fingerprint: u64,
        styles: Vec<Vec<Run>>,
    },
    /// Résultat de la sonde de joignabilité d'un serveur (version si connue, ou raison).
    Reach {
        server: String,
        url: String,
        result: Result<Option<String>, String>,
    },
    /// Résultat du login sur ce serveur : la session à conserver, ou le message d'erreur.
    LoggedIn {
        server: String,
        result: Result<Session, String>,
    },
}

/// Nombre maximal d'onglets : chacun garde ses pages (un log peut compter 50 000 lignes).
pub const MAX_TABS: usize = 10;

/// Ce qu'un onglet en arrière-plan signale dans la barre d'onglets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabMarker {
    /// Un job est en cours dans cet onglet.
    Running,
    /// Il s'est terminé avec ce statut.
    Done(Status),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabTitle {
    pub server: String,
    pub text: String,
    pub marker: Option<TabMarker>,
    pub active: bool,
}

/// Une page de la pile de navigation, avec son propre état de vue.
#[derive(Debug, Clone)]
pub enum Page {
    /// L'accueil : au fond de la pile, toujours.
    Home(HomeView),
    Repo(RepoView),
    Runs(RunsView),
    Logs(LogView),
    Browser(Browser),
}

impl Page {
    /// Copie pour un autre onglet (voir `Browser::fork`).
    fn fork(&self) -> Page {
        match self {
            Page::Browser(b) => Page::Browser(b.fork()),
            other => other.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoginField {
    Username,
    Password,
}

/// Le serveur répond-il sur le réseau ? Indépendant de la session : un serveur peut être
/// « connecté » (identifiants sauvegardés) et injoignable (VPN coupé).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    Checking,
    Up(Option<String>),
    Down(String),
}

/// Un serveur configuré, pour la liste des serveurs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerField {
    Name,
    Url,
    Token,
}

/// Formulaire d'ajout (`original` vide) ou de modification d'un serveur. Le nom ne change pas
/// à la modification : il sert de clé aux sessions, à l'historique et aux onglets.
#[derive(Debug)]
pub struct ServerForm {
    pub original: Option<String>,
    pub name: String,
    pub url: String,
    /// Vide = pas de token (ajout) ou token inchangé (modification).
    pub token: String,
    pub field: ServerField,
    pub error: Option<String>,
}

/// Ce que le formulaire demande à `main` de faire (écrire la config, créer le fournisseur).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDraft {
    pub original: Option<String>,
    pub name: String,
    pub url: String,
    pub token: Option<String>,
}

/// Formulaire de connexion. Le mot de passe n'y reste que le temps de la saisie.
#[derive(Debug)]
pub struct LoginForm {
    /// Serveur auquel on se connecte, et son libellé pour le titre.
    pub server: String,
    pub label: String,
    /// La connexion sert à changer d'onglet de serveur : une fois faite, l'onglet bascule ;
    /// annulée, il reste comme il était.
    pub switch_to: bool,
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
    /// Les serveurs configurés, pour la liste (`S`) et les libellés de connexion.
    pub servers: Vec<ServerEntry>,
    /// Liste des serveurs ouverte par-dessus l'interface (indice de la ligne choisie).
    pub server_picker: Option<usize>,
    /// Formulaire d'ajout / de modification d'un serveur, par-dessus la liste.
    pub server_form: Option<ServerForm>,
    /// Suppression en attente de confirmation (nom du serveur).
    pub server_delete: Option<String>,
    /// Message affiché dans la liste des serveurs (refus, confirmation).
    pub server_note: Option<String>,
    /// Une autre instance de smithy gère déjà la sauvegarde des onglets : celle-ci ne les
    /// restaure ni ne les sauvegarde (voir `workspace`).
    pub shared_instance: bool,
    /// Joignabilité des serveurs, mesurée tant que la liste est ouverte.
    pub reach: std::collections::HashMap<String, Reach>,
    probes: Vec<String>,
    /// Ce que sait faire chaque serveur **connecté** : `(logs, explorateur)`. Absent = pas de
    /// fournisseur (il faut se connecter avant de pouvoir y aller).
    caps: std::collections::HashMap<String, (bool, bool)>,
    /// Pages empilées de l'onglet affiché, comme dans un navigateur : `Échap` dépile et retrouve
    /// la page d'avant telle qu'on l'a laissée. Jamais vide : le fond est toujours l'accueil.
    pub stack: Vec<Page>,
    /// Les piles de tous les onglets, dans l'ordre ; à la place de l'onglet affiché (`active`)
    /// se trouve une pile vide, la sienne étant `stack`. Changer d'onglet échange les deux.
    tabs: Vec<Vec<Page>>,
    active: usize,
    /// Liste des onglets ouverte par-dessus l'interface (indice de la ligne choisie).
    pub tab_picker: Option<usize>,
    /// Serveur de l'onglet **affiché** (comme `stack`, c'est celui de l'onglet courant) : fil
    /// d'Ariane, historique, étiquette des requêtes. Chaque onglet a le sien, voir `tab_servers`.
    pub server_name: String,
    /// Le serveur de chaque onglet ; l'entrée de l'onglet affiché est mise à jour quand on le quitte.
    tab_servers: Vec<String>,
    /// Le serveur de l'onglet affiché sait-il fournir des logs ? (`Capabilities::logs`)
    pub logs_supported: bool,
    /// … et parcourir le code ? (`Capabilities::browse`)
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
    /// Chaque requête est étiquetée du serveur de l'onglet qui la demande.
    pending: Vec<(String, Request)>,
}

impl App {
    pub fn new() -> Self {
        let mut home = HomeView::new();
        let pending = home
            .request_current()
            .into_iter()
            .map(|r| (String::new(), r))
            .collect();
        Self {
            error: None,
            should_quit: false,
            login: None,
            servers: Vec::new(),
            server_picker: None,
            server_form: None,
            server_delete: None,
            server_note: None,
            shared_instance: false,
            reach: std::collections::HashMap::new(),
            probes: Vec::new(),
            caps: std::collections::HashMap::new(),
            stack: vec![Page::Home(home)],
            tabs: vec![Vec::new()],
            active: 0,
            tab_picker: None,
            server_name: String::new(),
            tab_servers: vec![String::new()],
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

    /// Les requêtes à lancer : celles de l'application, puis celles de chaque explorateur ouvert
    /// (dans n'importe quel onglet).
    #[cfg(test)]
    pub fn take_requests(&mut self) -> Vec<Request> {
        self.take_tagged_requests()
            .into_iter()
            .map(|(_, r)| r)
            .collect()
    }

    /// Comme `take_requests`, chaque requête portant le serveur de l'onglet qui la demande :
    /// c'est à ce fournisseur qu'il faut l'envoyer, et sa réponse y revient (`apply_for`).
    pub fn take_tagged_requests(&mut self) -> Vec<(String, Request)> {
        let mut out = std::mem::take(&mut self.pending);
        let active = self.active;
        let shown = (active, &mut self.stack);
        let others = self
            .tabs
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| *i != active);
        for (i, stack) in std::iter::once(shown).chain(others) {
            let server = if i == active {
                &self.server_name
            } else {
                &self.tab_servers[i]
            };
            for page in stack {
                if let Page::Browser(b) = page {
                    out.extend(b.take_requests().into_iter().map(|r| (server.clone(), r)));
                }
            }
        }
        out
    }

    /// Remet en file des requêtes que `main` n'a pas pu envoyer (serveur pas encore connecté).
    pub fn requeue(&mut self, requests: Vec<(String, Request)>) {
        self.pending.extend(requests);
    }

    fn queue(&mut self, req: Request) {
        self.pending.push((self.server_name.clone(), req));
    }

    fn queue_all(&mut self, reqs: impl IntoIterator<Item = Request>) {
        for r in reqs {
            self.queue(r);
        }
    }

    /// Les piles des seuls onglets qui sont sur ce serveur : deux serveurs peuvent avoir un
    /// dépôt du même nom, une réponse de l'un ne doit pas atteindre les onglets de l'autre.
    fn stacks_of(&mut self, server: &str) -> Vec<&mut Vec<Page>> {
        let active = self.active;
        let mut out = Vec::new();
        if self.server_name == server {
            out.push(&mut self.stack);
        }
        for (i, stack) in self.tabs.iter_mut().enumerate() {
            if i != active && self.tab_servers[i] == server {
                out.push(stack);
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
            self.queue(r);
        }
    }

    /// Entrée dans la saisie de l'accueil : « Explorer » lance sa recherche.
    pub fn home_commit(&mut self) {
        if let Some(r) = self.home_mut().and_then(HomeView::commit) {
            self.queue(r);
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
        self.queue(Request::RepoInfo(row.repo.clone()));
        self.queue(Request::Pipelines(row.repo));
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
            self.queue(Request::Pipelines(repo.clone()));
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
        self.queue(Request::Logs { repo, pipeline });
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
        self.queue(req);
    }

    /// Le job affiché tourne encore : ses logs sont à rafraîchir périodiquement. Son statut
    /// courant est celui de la liste des runs, plus bas dans la pile, qui se rafraîchit aussi.
    pub fn logs_are_live(&self) -> bool {
        live_in(&self.stack)
    }

    /// Toutes les 5 s : ce qui change tout seul sur la page affichée (les runs d'un dépôt, les
    /// logs d'un job en cours), et les jobs en cours des autres onglets, pour que leur titre
    /// signale la fin.
    pub fn tick(&mut self) {
        if self.server_picker.is_some() {
            self.probe_all();
        }
        match self.stack.last() {
            Some(Page::Runs(r)) => {
                let repo = r.repo.clone();
                self.queue(Request::Pipelines(repo));
            }
            Some(Page::Logs(v)) => {
                // La liste des runs, plus bas, dit si le job a fini.
                let repo = v.repo.clone();
                self.queue(Request::Pipelines(repo));
                if self.logs_are_live() {
                    self.refetch_logs();
                }
            }
            _ => {}
        }
        let mut background = Vec::new();
        for (i, stack) in self.tabs.iter().enumerate() {
            if i == self.active || !live_in(stack) {
                continue;
            }
            if let Some(Page::Logs(v)) = stack.last() {
                let server = self.tab_servers[i].clone();
                background.push((server.clone(), Request::Pipelines(v.repo.clone())));
                background.push((
                    server,
                    Request::Logs {
                        repo: v.repo.clone(),
                        pipeline: v.pipeline.clone(),
                    },
                ));
            }
        }
        self.pending.extend(background);
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
        self.queue_all(requests);
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
    pub fn open_login(&mut self, server: &str, username: &str, switch_to: bool) {
        let label = self
            .servers
            .iter()
            .find(|s| s.name == server)
            .map_or_else(|| server.to_owned(), |s| format!("{} ({})", s.name, s.url));
        self.login = Some(LoginForm {
            server: server.to_owned(),
            label,
            switch_to,
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

    /// Intègre une réponse du réseau. Elle est diffusée à tous les onglets : les pages
    /// concernées (même dépôt, même chemin, même job) la prennent, les autres l'écartent, ce qui
    /// règle aussi les réponses périmées (page dépilée, recherche abandonnée, autre dépôt).
    #[cfg(test)]
    pub fn apply(&mut self, msg: Msg) {
        let server = self.server_name.clone();
        self.apply_for(&server, msg);
    }

    /// Intègre la réponse d'un serveur : seuls les onglets de **ce** serveur la reçoivent
    /// (une réponse tardive, après un changement de serveur de l'onglet, est donc ignorée).
    pub fn apply_for(&mut self, server: &str, msg: Msg) {
        match msg {
            Msg::Reach {
                server: name,
                url,
                result,
            } => {
                // Une réponse pour une ancienne URL (serveur modifié depuis) est ignorée.
                if self.servers.iter().any(|s| s.name == name && s.url == url) {
                    self.reach.insert(
                        name,
                        match result {
                            Ok(v) => Reach::Up(v),
                            Err(e) => Reach::Down(e),
                        },
                    );
                }
            }
            Msg::Repos { source, result } => {
                for stack in self.stacks_of(server) {
                    if let Some(Page::Home(h)) = stack.first_mut() {
                        h.apply(&source, result.clone());
                    }
                }
            }
            Msg::RepoInfo { repo, result } => {
                for page in self.stacks_of(server).into_iter().flatten() {
                    if let Page::Repo(r) = page {
                        r.apply_info(&repo, result.clone());
                    }
                }
            }
            Msg::Pipelines { repo, result } => {
                for page in self.stacks_of(server).into_iter().flatten() {
                    match page {
                        Page::Repo(r) => r.apply_pipelines(&repo, &result),
                        Page::Runs(r) => r.apply_pipelines(&repo, result.clone()),
                        _ => {}
                    }
                }
            }
            Msg::Branches { repo, result } => {
                for page in self.stacks_of(server).into_iter().flatten() {
                    if let Page::Browser(b) = page {
                        b.on_branches(&repo, result.clone());
                    }
                }
            }
            Msg::Dir {
                repo,
                reference,
                path,
                result,
            } => {
                for page in self.stacks_of(server).into_iter().flatten() {
                    if let Page::Browser(b) = page {
                        b.on_dir(&repo, &reference, &path, result.clone());
                    }
                }
            }
            Msg::File {
                repo,
                reference,
                path,
                result,
            } => {
                for page in self.stacks_of(server).into_iter().flatten() {
                    if let Page::Browser(b) = page {
                        b.on_file(&repo, &reference, &path, result.clone());
                    }
                }
            }
            Msg::FileStyles {
                repo,
                reference,
                path,
                fingerprint,
                styles,
            } => {
                for page in self.stacks_of(server).into_iter().flatten() {
                    if let Page::Browser(b) = page {
                        b.on_styles(&repo, &reference, &path, fingerprint, styles.clone());
                    }
                }
            }
            Msg::Logs {
                repo,
                pipeline_id,
                result,
            } => {
                let size = self.view_size();
                let mut banner = None;
                for stack in self.stacks_of(server) {
                    if let Some(e) = apply_logs(stack, &repo, pipeline_id, &result, size) {
                        banner = Some(e);
                    }
                }
                if banner.is_some() {
                    self.error = banner;
                }
            }
            Msg::LoggedIn { server, result } => {
                let mine = self.login.as_ref().is_some_and(|f| f.server == server);
                match result {
                    Ok(_) if mine => {
                        let switch = self.login.take().is_some_and(|f| f.switch_to);
                        self.error = None;
                        if switch {
                            self.switch_server(&server);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        if let Some(f) = self.login.as_mut().filter(|_| mine) {
                            f.busy = false;
                            f.field = LoginField::Password;
                            f.error = Some(e);
                        }
                    }
                }
            }
        }
    }
}

// --- onglets

impl App {
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn stack_of(&self, i: usize) -> &[Page] {
        if i == self.active {
            &self.stack
        } else {
            &self.tabs[i]
        }
    }

    /// Les onglets dans l'ordre, avec leur titre et ce qu'ils signalent.
    pub fn tab_titles(&self) -> Vec<TabTitle> {
        (0..self.tabs.len())
            .map(|i| {
                let stack = self.stack_of(i);
                TabTitle {
                    server: if i == self.active {
                        self.server_name.clone()
                    } else {
                        self.tab_servers[i].clone()
                    },
                    text: stack.last().map_or_else(String::new, page_title),
                    marker: marker_of(stack),
                    active: i == self.active,
                }
            })
            .collect()
    }

    /// Passe à l'onglet `i` (les piles s'échangent : l'affichée reste dans `stack`).
    pub fn go_tab(&mut self, i: usize) {
        if i >= self.tabs.len() || i == self.active {
            return;
        }
        let leaving = std::mem::take(&mut self.stack);
        self.tabs[self.active] = leaving;
        self.tab_servers[self.active] = self.server_name.clone();
        self.stack = std::mem::take(&mut self.tabs[i]);
        self.active = i;
        self.sync_server();
    }

    /// Le serveur de l'onglet affiché devient « le serveur courant » : nom et capacités.
    fn sync_server(&mut self) {
        self.server_name = self.tab_servers[self.active].clone();
        let (logs, browse) = self
            .caps
            .get(&self.server_name)
            .copied()
            .unwrap_or_default();
        self.logs_supported = logs;
        self.browse_supported = browse;
    }

    /// `Tab` (`1`) et `Maj+Tab` (`-1`) : onglet suivant ou précédent, en boucle.
    pub fn switch_tab(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        if n > 1 {
            self.go_tab((self.active as isize + delta).rem_euclid(n) as usize);
        }
    }

    /// Touche `t` : ouvre la sélection dans un nouvel onglet. L'onglet courant est dupliqué
    /// (toutes ses pages, dans l'état où elles sont), puis la sélection y est ouverte ; sans
    /// sélection à ouvrir (logs, fichier), c'est une simple copie. Le nouvel onglet vient juste
    /// après l'actuel et devient l'onglet affiché.
    pub fn open_in_new_tab(&mut self) {
        if self.tabs.len() >= MAX_TABS {
            self.error = Some(format!("{MAX_TABS} onglets au maximum : fermez-en un (x)"));
            return;
        }
        let copy: Vec<Page> = self.stack.iter().map(Page::fork).collect();
        let at = self.active + 1;
        self.tabs.insert(at, copy);
        self.tab_servers.insert(at, self.server_name.clone());
        self.go_tab(at);
        match self.stack.last() {
            Some(Page::Home(_)) => self.home_enter(),
            Some(Page::Repo(_)) => self.repo_enter(),
            Some(Page::Runs(_)) => self.open_logs(),
            Some(Page::Browser(_)) => {
                if let Some(b) = self.browser_mut() {
                    b.enter();
                }
            }
            _ => {}
        }
    }

    /// Touche `x` : ferme l'onglet affiché. Le dernier redevient un accueil tout neuf.
    pub fn close_tab(&mut self) {
        self.close_tab_at(self.active);
    }

    pub fn close_tab_at(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            let mut home = HomeView::new();
            self.queue_all(home.request_current());
            self.stack = vec![Page::Home(home)];
            return;
        }
        if i == self.active {
            // On retombe sur l'onglet voisin (celui de gauche, sinon le premier).
            let next = if i > 0 { i - 1 } else { 1 };
            self.go_tab(next);
        }
        self.tabs.remove(i);
        self.tab_servers.remove(i);
        if i < self.active {
            self.active -= 1;
        }
    }

    // --- sauvegarde et restauration des onglets

    /// L'état à sauvegarder : pour chaque onglet son serveur et ses pages. Les logs ne sont pas
    /// gardés (ils demandent le run entier) : l'onglet revient à la liste des runs.
    pub fn snapshot(&self) -> SavedTabs {
        let tabs = (0..self.tabs.len())
            .map(|i| SavedTab {
                server: if i == self.active {
                    self.server_name.clone()
                } else {
                    self.tab_servers[i].clone()
                },
                pages: self
                    .stack_of(i)
                    .iter()
                    .filter_map(|p| match p {
                        Page::Home(h) => Some(SavedPage::Home {
                            section: h.section.index(),
                            selected: h.data().selected,
                        }),
                        Page::Repo(r) => Some(SavedPage::Repo {
                            repo: r.info.repo.clone(),
                            selected: r.selected,
                        }),
                        Page::Runs(r) => Some(SavedPage::Runs {
                            repo: r.repo.clone(),
                        }),
                        Page::Browser(b) => Some(SavedPage::Browser {
                            repo: b.repo.clone(),
                            reference: b.reference.clone(),
                            path: b.path.clone(),
                        }),
                        Page::Logs(_) => None,
                    })
                    .collect(),
            })
            .collect();
        SavedTabs::new(self.active, tabs)
    }

    /// Rouvre les onglets sauvegardés. Les serveurs qui n'existent plus sont écartés (avec leur
    /// onglet). Renvoie `false` s'il ne reste rien à restaurer : l'état n'a alors pas changé.
    /// Les données sont redemandées, chaque requête étiquetée du serveur de son onglet.
    pub fn restore(&mut self, saved: SavedTabs) -> bool {
        let known = |s: &str| self.servers.iter().any(|e| e.name == s);
        let kept: Vec<(usize, &SavedTab)> = saved
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| known(&t.server))
            .take(MAX_TABS)
            .collect();
        if kept.is_empty() {
            return false;
        }
        // L'onglet affiché : le même s'il reste, sinon le premier qui le suit (ou le dernier).
        let active = kept
            .iter()
            .position(|(i, _)| *i >= saved.active)
            .unwrap_or(kept.len() - 1);

        let mut requests = Vec::new();
        let mut stacks = Vec::new();
        let mut servers = Vec::new();
        for (_, tab) in &kept {
            let mut stack = Vec::new();
            for page in &tab.pages {
                match page {
                    SavedPage::Home { section, selected } => {
                        let mut h = HomeView::new();
                        h.restore(*section, *selected);
                        requests.extend(h.request_current().map(|r| (tab.server.clone(), r)));
                        stack.push(Page::Home(h));
                    }
                    SavedPage::Repo { repo, selected } => {
                        let mut v = RepoView::placeholder(repo.clone());
                        v.selected = *selected;
                        requests.push((tab.server.clone(), Request::RepoInfo(repo.clone())));
                        requests.push((tab.server.clone(), Request::Pipelines(repo.clone())));
                        stack.push(Page::Repo(v));
                    }
                    SavedPage::Runs { repo } => {
                        requests.push((tab.server.clone(), Request::Pipelines(repo.clone())));
                        stack.push(Page::Runs(RunsView::new(repo.clone())));
                    }
                    SavedPage::Browser {
                        repo,
                        reference,
                        path,
                    } => stack.push(Page::Browser(Browser::restore(
                        repo.clone(),
                        reference.clone(),
                        path.clone(),
                    ))),
                }
            }
            // Une pile commence toujours par l'accueil.
            if !matches!(stack.first(), Some(Page::Home(_))) {
                let mut h = HomeView::new();
                requests.extend(h.request_current().map(|r| (tab.server.clone(), r)));
                stack.insert(0, Page::Home(h));
            }
            stacks.push(stack);
            servers.push(tab.server.clone());
        }

        self.pending.clear();
        self.pending.extend(requests);
        self.tabs = vec![Vec::new(); stacks.len()];
        self.stack = std::mem::take(&mut stacks[active]);
        for (i, s) in stacks.into_iter().enumerate() {
            if i != active {
                self.tabs[i] = s;
            }
        }
        self.tab_servers = servers;
        self.active = active;
        self.sync_server();
        true
    }

    // --- serveurs

    /// Déclare les serveurs configurés (nom, URL), pour la liste et les libellés de connexion.
    pub fn set_servers(&mut self, servers: Vec<ServerEntry>) {
        self.servers = servers;
    }

    /// Un fournisseur est prêt pour ce serveur : ce qu'il sait faire. Le serveur est alors
    /// « connecté » (on peut y basculer sans login).
    pub fn set_caps(&mut self, server: &str, logs: bool, browse: bool) {
        self.caps.insert(server.to_owned(), (logs, browse));
        self.sync_server();
    }

    pub fn is_connected(&self, server: &str) -> bool {
        self.caps.contains_key(server)
    }

    /// Plusieurs serveurs configurés : les titres d'onglets disent de quel serveur ils sont.
    pub fn multi_server(&self) -> bool {
        self.servers.len() > 1
    }

    /// Change le serveur de l'onglet affiché : il garde son rang, sa pile repart sur l'accueil
    /// du nouveau serveur. Les autres onglets ne bougent pas, et une réponse tardive de l'ancien
    /// serveur sera ignorée (`apply_for`).
    pub fn switch_server(&mut self, server: &str) {
        if server == self.server_name {
            return;
        }
        self.server_name = server.to_owned();
        self.tab_servers[self.active] = server.to_owned();
        self.sync_server();
        let mut home = HomeView::new();
        self.queue_all(home.request_current());
        self.stack = vec![Page::Home(home)];
        self.error = None;
    }

    /// Serveur du premier onglet au démarrage : l'accueil déjà demandé lui revient.
    pub fn set_initial_server(&mut self, server: &str) {
        self.server_name = server.to_owned();
        self.tab_servers[self.active] = server.to_owned();
        for (tag, _) in self.pending.iter_mut().filter(|(t, _)| t.is_empty()) {
            *tag = server.to_owned();
        }
        self.sync_server();
    }

    pub fn open_server_picker(&mut self) {
        let at = self.servers.iter().position(|s| s.name == self.server_name);
        self.server_picker = Some(at.unwrap_or(0));
        self.server_note = None;
        for s in &self.servers {
            self.reach.entry(s.name.clone()).or_insert(Reach::Checking);
        }
        self.probe_all();
    }

    fn probe_all(&mut self) {
        let names: Vec<String> = self.servers.iter().map(|s| s.name.clone()).collect();
        for n in names {
            self.probe(&n);
        }
    }

    fn probe(&mut self, name: &str) {
        if !self.probes.iter().any(|p| p == name) {
            self.probes.push(name.to_owned());
        }
    }

    /// Les serveurs à sonder (nom, URL) : `main` lance une sonde pour chacun.
    pub fn take_probes(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.probes)
            .into_iter()
            .filter_map(|n| {
                let url = self.servers.iter().find(|s| s.name == n)?.url.clone();
                Some((n, url))
            })
            .collect()
    }

    pub fn close_server_picker(&mut self) {
        self.server_picker = None;
        self.server_delete = None;
        self.server_note = None;
    }

    /// Le serveur est-il celui d'un onglet ouvert ?
    fn server_in_use(&self, name: &str) -> bool {
        self.server_name == name || self.tab_servers.iter().any(|s| s == name)
    }

    /// Oublie ce qu'on savait d'un serveur (il n'est plus connecté).
    pub fn forget_caps(&mut self, server: &str) {
        self.caps.remove(server);
        self.sync_server();
    }

    // --- ajout, modification et suppression

    /// `a` dans la liste : formulaire vide.
    pub fn server_form_add(&mut self) {
        self.server_note = None;
        self.server_form = Some(ServerForm {
            original: None,
            name: String::new(),
            url: "https://".to_owned(),
            token: String::new(),
            field: ServerField::Name,
            error: None,
        });
    }

    /// `e` dans la liste : formulaire prérempli avec le serveur choisi (le nom n'est pas modifiable).
    pub fn server_form_edit(&mut self) {
        self.server_note = None;
        let Some(entry) = self
            .server_picker
            .and_then(|i| self.servers.get(i))
            .cloned()
        else {
            return;
        };
        self.server_form = Some(ServerForm {
            original: Some(entry.name.clone()),
            name: entry.name,
            url: entry.url,
            token: String::new(),
            field: ServerField::Url,
            error: None,
        });
    }

    fn server_form_field(&mut self) -> Option<&mut String> {
        let f = self.server_form.as_mut()?;
        Some(match f.field {
            ServerField::Name => &mut f.name,
            ServerField::Url => &mut f.url,
            ServerField::Token => &mut f.token,
        })
    }

    pub fn server_form_type(&mut self, c: char) {
        if let Some(v) = self.server_form_field() {
            v.push(c);
        }
    }

    pub fn server_form_backspace(&mut self) {
        if let Some(v) = self.server_form_field() {
            v.pop();
        }
    }

    /// Tab / flèches : champ suivant (`1`) ou précédent (`-1`) ; le nom est sauté à la modification.
    pub fn server_form_move(&mut self, delta: isize) {
        let Some(f) = self.server_form.as_mut() else {
            return;
        };
        let all = [ServerField::Name, ServerField::Url, ServerField::Token];
        let usable: Vec<ServerField> = all
            .into_iter()
            .filter(|x| f.original.is_none() || *x != ServerField::Name)
            .collect();
        let at = usable.iter().position(|x| *x == f.field).unwrap_or(0) as isize;
        f.field = usable[(at + delta).rem_euclid(usable.len() as isize) as usize];
    }

    /// Entrée : passe au champ suivant, ou valide sur le dernier. Renvoie ce qu'il faut
    /// enregistrer si la saisie est correcte ; sinon l'erreur reste dans le formulaire.
    pub fn server_form_enter(&mut self) -> Option<ServerDraft> {
        let f = self.server_form.as_mut()?;
        if f.field != ServerField::Token {
            self.server_form_move(1);
            return None;
        }
        let others: Vec<&str> = self
            .servers
            .iter()
            .map(|s| s.name.as_str())
            .filter(|n| Some(*n) != f.original.as_deref())
            .collect();
        let (name, url) = (f.name.trim().to_owned(), f.url.trim().trim_end_matches('/'));
        if let Err(e) = crate::config::check_server(&name, url, &others) {
            // Le curseur retourne sur le champ fautif (le nom n'est modifiable qu'à l'ajout).
            let on_name = f.original.is_none() && (e.starts_with("nom") || e.starts_with("le nom"));
            f.field = if on_name {
                ServerField::Name
            } else {
                ServerField::Url
            };
            f.error = Some(e);
            return None;
        }
        let token = f.token.trim();
        Some(ServerDraft {
            original: f.original.clone(),
            name,
            url: url.to_owned(),
            token: (!token.is_empty()).then(|| token.to_owned()),
        })
    }

    /// `main` n'a pas pu enregistrer (disque, droits) : le formulaire reste ouvert.
    pub fn server_form_fail(&mut self, error: String) {
        if let Some(f) = self.server_form.as_mut() {
            f.error = Some(error);
        }
    }

    /// Enregistrement réussi : le formulaire se ferme, la liste reste sur ce serveur.
    pub fn server_form_done(&mut self, servers: Vec<ServerEntry>, name: &str) {
        self.server_form = None;
        self.servers = servers;
        // L'URL a pu changer : on remesure.
        self.reach.insert(name.to_owned(), Reach::Checking);
        self.probe(name);
        if let Some(i) = self.servers.iter().position(|s| s.name == name) {
            self.server_picker = Some(i);
        }
    }

    pub fn server_form_cancel(&mut self) {
        self.server_form = None;
    }

    /// `d` dans la liste : demande confirmation, sauf si la suppression est refusée.
    pub fn server_delete_ask(&mut self) {
        self.server_note = None;
        let Some(name) = self
            .server_picker
            .and_then(|i| self.servers.get(i))
            .map(|s| s.name.clone())
        else {
            return;
        };
        if self.servers.len() <= 1 {
            self.server_note = Some("impossible de supprimer le dernier serveur".to_owned());
        } else if self.server_in_use(&name) {
            self.server_note = Some(format!(
                "« {name} » est utilisé par un onglet : changez-le de serveur d'abord"
            ));
        } else {
            self.server_delete = Some(name);
        }
    }

    /// `o` / `y` : confirme. Renvoie le serveur à supprimer pour que `main` le retire de la config.
    pub fn server_delete_confirm(&mut self) -> Option<String> {
        self.server_delete.take()
    }

    /// Après la suppression par `main` : la liste est mise à jour.
    pub fn server_delete_done(&mut self, servers: Vec<ServerEntry>) {
        self.servers = servers;
        let names: Vec<&str> = self.servers.iter().map(|s| s.name.as_str()).collect();
        self.reach.retain(|n, _| names.contains(&n.as_str()));
        let max = self.servers.len().saturating_sub(1);
        self.server_picker = self.server_picker.map(|i| i.min(max));
        self.server_note = None;
    }

    pub fn server_delete_cancel(&mut self) {
        self.server_delete = None;
    }

    pub fn server_picker_move(&mut self, delta: isize) {
        if let Some(sel) = self.server_picker.as_mut() {
            let max = self.servers.len().saturating_sub(1);
            *sel = sel.saturating_add_signed(delta).min(max);
        }
    }

    /// Entrée dans la liste : bascule tout de suite si le serveur est connecté ; sinon renvoie
    /// son nom pour que `main` retrouve un identifiant et ouvre le login (la bascule se fera
    /// une fois connecté).
    pub fn server_picker_choose(&mut self) -> Option<String> {
        let name = self.servers.get(self.server_picker.take()?)?.name.clone();
        if name == self.server_name {
            return None;
        }
        if self.is_connected(&name) {
            self.switch_server(&name);
            None
        } else {
            Some(name)
        }
    }

    /// Échap dans le formulaire. Renvoie `true` s'il faut quitter : seulement quand il s'agit
    /// de la connexion de démarrage (le serveur de l'onglet n'est pas connecté, rien à voir
    /// sans elle). Une bascule annulée ou un renouvellement de session se contentent de fermer.
    pub fn login_escape(&mut self) -> bool {
        let Some(form) = self.login.take() else {
            return false;
        };
        self.login_quits(&form)
    }

    /// Échap sur ce formulaire quitterait-il ? Seulement si aucun serveur n'est connecté :
    /// il n'y aurait alors rien à afficher.
    pub fn login_quits(&self, form: &LoginForm) -> bool {
        !form.switch_to && self.caps.is_empty()
    }

    // --- liste des onglets (touche `T`)

    pub fn open_tab_picker(&mut self) {
        self.tab_picker = Some(self.active);
    }

    pub fn tab_picker_move(&mut self, delta: isize) {
        if let Some(sel) = self.tab_picker.as_mut() {
            let max = self.tabs.len().saturating_sub(1);
            *sel = sel.saturating_add_signed(delta).min(max);
        }
    }

    /// Entrée dans la liste : va à l'onglet choisi et la ferme.
    pub fn tab_picker_go(&mut self) {
        if let Some(i) = self.tab_picker.take() {
            self.go_tab(i);
        }
    }

    /// `x` dans la liste : ferme l'onglet choisi, la liste reste ouverte.
    pub fn tab_picker_close(&mut self) {
        if let Some(i) = self.tab_picker {
            self.close_tab_at(i);
            self.tab_picker = Some(i.min(self.tabs.len().saturating_sub(1)));
        }
    }
}

/// Titre court d'une page, pour la barre d'onglets.
fn page_title(page: &Page) -> String {
    match page {
        Page::Home(_) => "Accueil".to_owned(),
        Page::Repo(r) => r.info.repo.name.clone(),
        Page::Runs(r) => format!("{} · CI", r.repo.name),
        Page::Logs(v) => format!("{} · {}", v.repo.name, v.pipeline.job),
        Page::Browser(b) => {
            let leaf = match &b.mode {
                crate::browser::Mode::File(fv) => fv.path.as_str(),
                _ => b.path.as_str(),
            };
            let leaf = leaf.rsplit('/').find(|s| !s.is_empty()).unwrap_or("Code");
            format!("{} · {leaf}", b.repo.name)
        }
    }
}

/// Dans la pile d'un onglet, le job affiché tourne-t-il encore ? Son statut courant est celui
/// de la liste des runs, plus bas dans la pile, qui se rafraîchit aussi.
fn live_in(stack: &[Page]) -> bool {
    let Some(Page::Logs(v)) = stack.last() else {
        return false;
    };
    let current = stack
        .iter()
        .find_map(|p| match p {
            Page::Runs(r) if r.repo == v.repo => r.pipelines.iter().find(|x| x.id == v.pipeline.id),
            _ => None,
        })
        .map_or(v.pipeline.status, |p| p.status);
    matches!(current, Status::Running | Status::Pending)
}

/// Un onglet qui montre des logs signale si le job tourne encore, ou comment il a fini.
fn marker_of(stack: &[Page]) -> Option<TabMarker> {
    let Some(Page::Logs(v)) = stack.last() else {
        return None;
    };
    if live_in(stack) {
        return Some(TabMarker::Running);
    }
    let status = stack
        .iter()
        .find_map(|p| match p {
            Page::Runs(r) if r.repo == v.repo => r.pipelines.iter().find(|x| x.id == v.pipeline.id),
            _ => None,
        })
        .map_or(v.pipeline.status, |p| p.status);
    Some(TabMarker::Done(status))
}

/// Applique une réponse de logs à la page de logs d'une pile, si c'est bien celle de ce job.
/// Renvoie le message à afficher quand un rafraîchissement échoue (le contenu reste en place).
fn apply_logs(
    stack: &mut [Page],
    repo: &Repo,
    pipeline_id: u64,
    result: &Result<JobLog, Error>,
    (w, h): (usize, usize),
) -> Option<String> {
    let live = live_in(stack);
    let Some(Page::Logs(v)) = stack.last_mut() else {
        return None;
    };
    // Réponse périmée : la vue a été fermée ou ouverte sur un autre job.
    if v.repo != *repo || v.pipeline.id != pipeline_id {
        return None;
    }
    match result {
        Ok(log) => {
            let first_load = matches!(v.state, LogState::Loading);
            let was_at_bottom = v.scroll >= v.max_scroll(w, h);
            v.notice = log.notice.clone();
            v.set_log(log);
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
            None
        }
        Err(e) => {
            if matches!(v.state, LogState::Ready(_)) {
                // On garde ce qu'on affiche, l'échec va dans le bandeau.
                Some(e.to_string())
            } else {
                v.state = LogState::Failed(e.to_string());
                None
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
        a.set_caps("", true, true);
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
        b.set_caps("", false, b.browse_supported);
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
        a.open_login("", "", false);
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
        a.open_login("", "thomas", false);
        assert_eq!(a.login.as_ref().unwrap().field, LoginField::Password);
    }

    #[test]
    fn champs_vides_refuses() {
        let mut a = App::new();
        a.open_login("", "thomas", false);
        assert!(a.login_enter().is_none());
        let f = a.login.as_ref().unwrap();
        assert!(!f.busy && f.error.is_some());
    }

    #[test]
    fn echec_de_login_rouvre_la_saisie() {
        let mut a = App::new();
        a.open_login("", "thomas", false);
        typed(&mut a, "mauvais");
        a.login_enter().unwrap();
        a.apply(Msg::LoggedIn {
            server: String::new(),
            result: Err("identifiant ou mot de passe incorrect".into()),
        });
        let f = a.login.as_ref().unwrap();
        assert!(!f.busy);
        assert_eq!(f.field, LoginField::Password);
        assert!(f.error.as_deref().unwrap().contains("incorrect"));
    }

    #[test]
    fn succes_de_login_ferme_le_formulaire() {
        let mut a = App::new();
        a.open_login("", "thomas", false);
        let session = Session {
            url: "http://a".into(),
            username: "thomas".into(),
            token: Some("t".into()),
            cookies: Default::default(),
        };
        a.apply(Msg::LoggedIn {
            server: String::new(),
            result: Ok(session),
        });
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
        a.set_caps("", a.logs_supported, false);
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

    // --- onglets

    fn titles(a: &App) -> Vec<String> {
        a.tab_titles().into_iter().map(|t| t.text).collect()
    }

    fn active_index(a: &App) -> usize {
        a.tab_titles().iter().position(|t| t.active).unwrap()
    }

    /// Accueil avec deux dépôts (`a`, `b`) chargés, un seul onglet.
    fn home_with_repos() -> App {
        let mut a = fresh();
        a.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info("a"), info("b")]),
        });
        a
    }

    #[test]
    fn un_seul_onglet_au_depart() {
        let a = App::new();
        assert_eq!(a.tab_count(), 1);
        assert_eq!(
            a.tab_titles(),
            [TabTitle {
                server: String::new(),
                text: "Accueil".into(),
                marker: None,
                active: true
            }]
        );
        let mut b = App::new();
        b.switch_tab(1); // sans autre onglet : sans effet
        b.switch_tab(-1);
        b.go_tab(3);
        assert_eq!((b.tab_count(), active_index(&b)), (1, 0));
    }

    #[test]
    fn t_sur_l_accueil_ouvre_le_depot_dans_un_nouvel_onglet() {
        let mut a = home_with_repos();
        a.list_move(1); // « b »
        a.open_in_new_tab();
        assert_eq!(a.tab_count(), 2);
        assert_eq!(
            (titles(&a), active_index(&a)),
            (vec!["Accueil".to_owned(), "b".to_owned()], 1)
        );
        assert!(
            a.repo_page().is_some(),
            "le nouvel onglet est affiché, sur la page du dépôt"
        );
        assert_eq!(a.stack.len(), 2);
        assert_eq!(
            a.take_requests(),
            [Request::RepoInfo(repo("b")), Request::Pipelines(repo("b"))]
        );
        // L'onglet d'origine est resté sur l'accueil, sélection comprise
        a.switch_tab(-1);
        assert!(a.home().is_some() && a.stack.len() == 1);
        assert_eq!(a.home().unwrap().selected(2), 1);
        // L'ouverture est aussi dans l'historique
        assert_eq!(a.recents[0].name, "b");
    }

    #[test]
    fn les_onglets_sont_independants() {
        let mut a = home_with_repos();
        a.open_in_new_tab(); // onglet 2 : dépôt « a »
        a.repo_enter(); // … puis la CI
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![pipe(7)]),
        });
        a.open_logs();
        assert_eq!(a.stack.len(), 4);
        // Onglet 1 : toujours l'accueil, on peut y naviguer sans toucher à l'autre
        a.switch_tab(-1);
        assert_eq!(a.stack.len(), 1);
        a.home_switch(1); // « Suivis »
        a.switch_tab(1);
        assert_eq!(a.stack.len(), 4, "l'autre onglet n'a pas bougé");
        assert!(a.logs().is_some());
        a.close_logs();
        a.pop();
        assert_eq!(a.stack.len(), 2);
        a.switch_tab(1); // boucle : retour à l'onglet 1
        assert_eq!(a.home().unwrap().section, crate::pages::Section::Watched);
        assert_eq!(a.stack.len(), 1);
    }

    #[test]
    fn t_selon_la_page() {
        // Page du dépôt : la rubrique choisie s'ouvre dans le nouvel onglet
        let mut a = app_on_repo();
        a.open_in_new_tab();
        assert!(a.runs().is_some(), "« CI » est sélectionnée");
        a.switch_tab(-1);
        assert!(
            a.repo_page().is_some(),
            "l'original reste sur la page du dépôt"
        );
        a.list_move(1);
        a.open_in_new_tab();
        assert!(a.browser().is_some(), "« Code » dans un autre onglet");
        // Runs : le run sélectionné ouvre ses logs dans un nouvel onglet
        let mut r = app_on_run(Status::Success);
        r.open_in_new_tab();
        assert!(r.logs().is_some());
        r.switch_tab(-1);
        assert!(r.runs().is_some() && r.logs().is_none());
        // Logs : simple copie de l'onglet
        let mut l = app_on_run(Status::Success);
        l.open_logs();
        let depth = l.stack.len();
        l.open_in_new_tab();
        assert_eq!((l.tab_count(), l.stack.len()), (2, depth));
        assert!(l.logs().is_some());
    }

    #[test]
    fn t_dans_l_explorateur_ouvre_l_entree_dans_un_nouvel_onglet() {
        let mut a = explorer();
        feed(&mut a);
        a.list_move(0);
        a.open_in_new_tab(); // la 1re entrée est un dossier : « src »
        assert_eq!(a.browser().unwrap().path, "src");
        a.switch_tab(-1);
        assert_eq!(
            a.browser().unwrap().path,
            "",
            "l'original est resté à la racine"
        );
        // Un explorateur copié ne redemande pas ce qui était déjà en attente
        let mut b = explorer(); // requête « branches » encore en file
        b.open_in_new_tab();
        let branches = b
            .take_requests()
            .into_iter()
            .filter(|r| matches!(r, Request::Branches(_)))
            .count();
        assert_eq!(branches, 1, "une seule demande pour les deux onglets");
    }

    #[test]
    fn limite_d_onglets() {
        let mut a = home_with_repos();
        for _ in 0..MAX_TABS + 3 {
            a.open_in_new_tab();
            if a.tab_count() < MAX_TABS {
                a.pop();
            }
        }
        assert_eq!(a.tab_count(), MAX_TABS);
        assert!(a.error.as_deref().unwrap().contains("onglets au maximum"));
    }

    #[test]
    fn changer_d_onglet_en_boucle() {
        let mut a = home_with_repos();
        a.open_in_new_tab();
        a.pop();
        a.open_in_new_tab();
        a.pop();
        assert_eq!(a.tab_count(), 3);
        assert_eq!(active_index(&a), 2);
        a.switch_tab(1);
        assert_eq!(active_index(&a), 0, "après le dernier, le premier");
        a.switch_tab(-1);
        assert_eq!(active_index(&a), 2, "avant le premier, le dernier");
        a.go_tab(1);
        assert_eq!(active_index(&a), 1);
        a.go_tab(1); // déjà là
        a.go_tab(9); // inexistant
        assert_eq!(active_index(&a), 1);
    }

    #[test]
    fn fermer_des_onglets() {
        let mut a = home_with_repos();
        a.list_move(1);
        a.open_in_new_tab(); // « b »
        a.switch_tab(-1);
        a.list_move(-1);
        a.open_in_new_tab(); // « a » juste après l'accueil : [Accueil, a, b]
        assert_eq!(titles(&a), ["Accueil", "a", "b"]);
        assert_eq!(active_index(&a), 1);
        // Fermer l'onglet affiché : on retombe à gauche
        a.close_tab();
        assert_eq!(
            (titles(&a), active_index(&a)),
            (vec!["Accueil".to_owned(), "b".to_owned()], 0)
        );
        // Fermer un onglet à gauche de l'affiché décale l'indice sans changer d'onglet
        a.go_tab(1);
        a.close_tab_at(0);
        assert_eq!((titles(&a), active_index(&a)), (vec!["b".to_owned()], 0));
        assert!(a.repo_page().is_some());
        // Le dernier onglet redevient un accueil tout neuf
        a.take_requests();
        a.close_tab();
        assert_eq!((a.tab_count(), titles(&a)), (1, vec!["Accueil".to_owned()]));
        assert!(a.home().is_some());
        assert_eq!(a.take_requests(), [Request::Repos(RepoSource::Mine)]);
        a.close_tab_at(5); // inexistant : sans effet
    }

    #[test]
    fn les_reponses_atteignent_tous_les_onglets() {
        let mut a = app_on_run(Status::Success);
        a.open_in_new_tab(); // onglet 2 : les logs du run 7
        a.take_requests();
        a.switch_tab(-1); // retour sur l'onglet 1 (liste des runs)
        // Une liste arrive pendant qu'on est sur l'onglet 1 : l'onglet 2 la reçoit aussi
        let mut done = pipe(7);
        done.status = Status::Failure;
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![done, pipe(8)]),
        });
        assert_eq!(a.runs().unwrap().pipelines.len(), 2);
        a.switch_tab(1);
        a.pop(); // sous les logs de l'onglet 2 : sa propre liste des runs
        assert_eq!(
            a.runs().unwrap().pipelines.len(),
            2,
            "l'onglet en arrière-plan a été mis à jour"
        );
        // Des logs arrivent pour l'onglet 2 alors qu'on est ailleurs
        let mut b = app_on_run(Status::Success);
        b.open_logs();
        b.open_in_new_tab(); // copie de l'onglet des logs
        b.switch_tab(-1);
        deliver(
            &mut b,
            7,
            Ok(JobLog {
                steps: vec![step("s", Status::Failure, 2)],
                ..Default::default()
            }),
        );
        b.switch_tab(1);
        assert!(
            matches!(b.logs().unwrap().state, LogState::Ready(_)),
            "la copie reçoit aussi les logs"
        );
        // Une recherche d'accueil aboutit dans l'onglet où elle a été lancée, même en arrière-plan
        let mut c = fresh();
        c.home_switch(3);
        c.home_mut().unwrap().start_edit();
        "tree"
            .chars()
            .for_each(|ch| c.home_mut().unwrap().type_char(ch));
        c.home_commit();
        c.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info("zz")]),
        });
        c.open_in_new_tab(); // copie de l'accueil, sans sélection à ouvrir
        c.apply(Msg::Repos {
            source: RepoSource::Search("tree".into()),
            result: Ok(vec![info("tree")]),
        });
        c.switch_tab(-1);
        assert_eq!(c.home_rows().len(), 1);
    }

    #[test]
    fn marqueurs_des_onglets() {
        let mut a = app_on_run(Status::Running);
        assert_eq!(
            a.tab_titles()[0].marker,
            None,
            "pas de marqueur hors des logs"
        );
        a.open_logs();
        assert_eq!(a.tab_titles()[0].marker, Some(TabMarker::Running));
        a.open_in_new_tab(); // copie de l'onglet des logs
        a.switch_tab(-1);
        a.pop(); // l'onglet 1 quitte les logs
        assert_eq!(a.tab_titles()[0].marker, None);
        assert_eq!(a.tab_titles()[1].marker, Some(TabMarker::Running));
        // Le job se termine : la liste des runs, sous les logs de l'onglet 2, le dit
        let mut failed = pipe(7);
        failed.status = Status::Failure;
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![failed]),
        });
        assert_eq!(
            a.tab_titles()[1].marker,
            Some(TabMarker::Done(Status::Failure))
        );
        assert_eq!(titles(&a), ["a · CI", "a · build"]);
    }

    #[test]
    fn tick_des_onglets_en_arriere_plan() {
        let mut a = app_on_run(Status::Running);
        a.open_logs();
        a.open_in_new_tab(); // onglet 2 = copie ; on reste dessus
        a.take_requests();
        a.pop(); // l'onglet affiché quitte les logs : seul l'onglet 1 suit encore le job
        a.take_requests();
        a.tick();
        let reqs = a.take_requests();
        assert!(
            reqs.iter().any(|r| matches!(r, Request::Logs { .. })),
            "job en cours d'un autre onglet : ses logs sont suivis"
        );
        assert!(reqs.contains(&Request::Pipelines(repo("a"))));
        // Une fois le job fini, plus rien à suivre en arrière-plan
        let mut done = pipe(7);
        done.status = Status::Success;
        a.apply(Msg::Pipelines {
            repo: repo("a"),
            result: Ok(vec![done]),
        });
        a.tick();
        assert!(
            !a.take_requests()
                .iter()
                .any(|r| matches!(r, Request::Logs { .. }))
        );
    }

    #[test]
    fn titres_des_pages() {
        let mut a = app_on_run(Status::Success);
        assert_eq!(titles(&a), ["a · CI"]);
        a.pop();
        assert_eq!(titles(&a), ["a"]);
        a.list_move(1);
        a.repo_enter();
        assert_eq!(titles(&a), ["a · Code"]);
        feed(&mut a);
        a.browser_mut().unwrap().enter(); // dossier « src »
        a.apply(Msg::Dir {
            repo: repo("a"),
            reference: "main".into(),
            path: "src".into(),
            result: Ok(vec![]),
        });
        assert_eq!(titles(&a), ["a · src"]);
    }

    #[test]
    fn liste_des_onglets() {
        let mut a = home_with_repos();
        a.open_in_new_tab();
        a.pop();
        a.open_in_new_tab();
        a.pop();
        a.go_tab(0);
        assert_eq!(a.tab_picker, None);
        a.open_tab_picker();
        assert_eq!(a.tab_picker, Some(0), "ouverte sur l'onglet affiché");
        a.tab_picker_move(2);
        a.tab_picker_move(5);
        assert_eq!(a.tab_picker, Some(2));
        a.tab_picker_go();
        assert_eq!((a.tab_picker, active_index(&a)), (None, 2));
        // Fermer depuis la liste : elle reste ouverte
        a.open_tab_picker();
        a.tab_picker_move(-1);
        a.tab_picker_close();
        assert_eq!(a.tab_count(), 2);
        assert_eq!(a.tab_picker, Some(1));
        a.tab_picker_close();
        assert_eq!(a.tab_picker, Some(0));
        a.tab_picker_close(); // le dernier redevient un accueil
        assert_eq!(a.tab_count(), 1);
        a.tab_picker_move(1);
        a.tab_picker_go();
        assert!(a.tab_picker.is_none());
    }

    #[test]
    fn requetes_d_un_explorateur_d_un_autre_onglet() {
        let mut a = app_on_repo();
        a.list_move(1);
        a.repo_enter(); // explorateur ouvert : « branches » en attente, non prise
        a.open_in_new_tab(); // copie affichée ; l'explorateur d'origine est dans l'onglet 1, inactif
        let reqs = a.take_requests();
        assert_eq!(
            reqs.iter()
                .filter(|r| matches!(r, Request::Branches(_)))
                .count(),
            1,
            "la demande de l'onglet en arrière-plan part, une seule fois (la copie n'en redemande pas)"
        );
        // Dépiler la page d'un explorateur abandonne aussi sa requête non lancée
        let mut b = app_on_repo();
        b.list_move(1);
        b.repo_enter();
        b.pop();
        assert!(b.take_requests().is_empty());
    }

    // --- un serveur par onglet

    fn entry(n: &str) -> ServerEntry {
        ServerEntry {
            name: n.into(),
            url: format!("http://{n}"),
        }
    }

    /// Deux onglets sur le serveur `A` (liste des runs de `o/a`, run 7), deux serveurs connus et
    /// connectés ; l'onglet 2 est affiché.
    fn two_tabs_on_a() -> App {
        let mut a = app_on_run(Status::Running);
        a.set_initial_server("A");
        a.set_servers(vec![entry("A"), entry("B")]);
        a.set_caps("A", true, true);
        a.set_caps("B", true, false);
        a.open_in_new_tab();
        a.take_requests();
        a
    }

    fn run_count(a: &App) -> usize {
        a.runs().map_or(0, |r| r.pipelines.len())
    }

    #[test]
    fn changer_de_serveur_ne_touche_pas_les_autres_onglets() {
        let mut a = two_tabs_on_a();
        assert_eq!(a.tab_count(), 2);
        a.switch_server("B");
        assert_eq!(a.server_name, "B");
        assert!(a.home().is_some() && a.stack.len() == 1, "accueil neuf");
        assert!((a.logs_supported, a.browse_supported) == (true, false));
        // Le rang est conservé, l'autre onglet est intact et reste sur A
        assert_eq!(active_index(&a), 1);
        a.switch_tab(-1);
        assert_eq!(a.server_name, "A");
        assert_eq!(run_count(&a), 1);
        assert!(a.browse_supported);
        assert_eq!(
            a.tab_titles()
                .iter()
                .map(|t| t.server.as_str())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
    }

    #[test]
    fn la_demande_d_accueil_du_nouveau_serveur_est_etiquetee() {
        let mut a = two_tabs_on_a();
        a.switch_server("B");
        let reqs = a.take_tagged_requests();
        assert!(!reqs.is_empty());
        assert!(reqs.iter().all(|(s, _)| s == "B"), "{reqs:?}");
    }

    #[test]
    fn une_reponse_n_atteint_que_les_onglets_de_son_serveur() {
        // Les deux serveurs ont un dépôt `o/a` : sans étiquette, les runs de l'un iraient chez l'autre.
        let mut a = two_tabs_on_a();
        a.switch_tab(-1); // onglet 1 sur A
        a.open_in_new_tab(); // onglet 2 = copie, sur A
        a.take_requests();
        a.switch_server("B");
        a.switch_tab(-1); // retour sur l'onglet 1 (A)
        assert_eq!(a.tab_count(), 3);
        let two = |n: u64| Ok(vec![pipe(7), pipe(n)]);
        a.apply_for(
            "B",
            Msg::Pipelines {
                repo: repo("a"),
                result: two(8),
            },
        );
        assert_eq!(run_count(&a), 1, "la réponse de B ne touche pas A");
        a.apply_for(
            "A",
            Msg::Pipelines {
                repo: repo("a"),
                result: two(9),
            },
        );
        assert_eq!(run_count(&a), 2);
        // L'onglet 2 est passé sur B : rien reçu. L'onglet 3 (copie, resté sur A, ouvert sur
        // les logs) l'a reçue aussi.
        a.switch_tab(1);
        assert_eq!((a.server_name.as_str(), run_count(&a)), ("B", 0));
        a.switch_tab(1);
        assert_eq!(a.server_name, "A");
        a.pop();
        assert_eq!(run_count(&a), 2);
    }

    #[test]
    fn reponse_tardive_apres_changement_de_serveur_ignoree() {
        let mut a = two_tabs_on_a();
        // L'onglet 2 demande les runs de A, puis passe sur B avant la réponse
        a.switch_server("B");
        a.apply_for(
            "A",
            Msg::Repos {
                source: RepoSource::Mine,
                result: Ok(vec![info("zzz")]),
            },
        );
        assert!(
            a.home_rows().iter().all(|r| r.repo.name != "zzz"),
            "les dépôts de A n'apparaissent pas dans l'accueil de B"
        );
    }

    #[test]
    fn les_requetes_d_arriere_plan_portent_le_serveur_de_leur_onglet() {
        let mut a = two_tabs_on_a();
        a.open_logs(); // onglet 2 (A) suit un job en cours
        a.take_requests();
        a.switch_server("B"); // l'onglet 2 passe sur B (accueil)
        a.take_requests();
        a.switch_tab(-1); // onglet 1 (A) affiché, en logs ? non : liste des runs
        a.open_logs();
        a.take_requests();
        a.switch_tab(1);
        a.tick();
        let reqs = a.take_tagged_requests();
        assert!(
            reqs.iter()
                .filter(|(_, r)| matches!(r, Request::Logs { .. }))
                .all(|(s, _)| s == "A"),
            "{reqs:?}"
        );
        assert!(
            reqs.iter()
                .any(|(s, r)| s == "A" && matches!(r, Request::Logs { .. }))
        );
    }

    #[test]
    fn les_requetes_d_un_explorateur_inactif_portent_son_serveur() {
        let mut a = app_on_repo();
        a.set_initial_server("A");
        a.set_servers(vec![entry("A"), entry("B")]);
        a.set_caps("A", true, true);
        a.set_caps("B", true, true);
        a.list_move(1);
        a.repo_enter(); // CI
        a.pop();
        // Code : ouvre l'explorateur dans l'onglet 1, puis onglet 2 sur B
        a.list_move(1);
        a.repo_enter();
        assert!(a.browser().is_some());
        a.open_in_new_tab();
        a.switch_server("B");
        a.switch_tab(-1);
        let reqs = a.take_tagged_requests();
        assert!(
            reqs.iter()
                .filter(|(_, r)| matches!(r, Request::Branches(_)))
                .all(|(s, _)| s == "A"),
            "{reqs:?}"
        );
    }

    #[test]
    fn requeue_remet_en_file() {
        let mut a = fresh();
        a.switch_server("B");
        let reqs = a.take_tagged_requests();
        assert!(!reqs.is_empty());
        a.requeue(reqs.clone());
        assert_eq!(a.take_tagged_requests(), reqs);
    }

    #[test]
    fn capacites_par_serveur() {
        let mut a = fresh();
        a.set_servers(vec![entry("A"), entry("B")]);
        a.set_caps("A", true, true);
        a.set_caps("B", false, true);
        a.switch_server("A");
        assert!(a.logs_supported);
        a.switch_server("B");
        assert!(!a.logs_supported && a.browse_supported);
        a.close_tab(); // le dernier onglet : reste sur B
        assert!(!a.logs_supported);
        assert!(a.is_connected("A") && !a.is_connected("C"));
    }

    #[test]
    fn nouvel_onglet_herite_du_serveur_et_la_fermeture_reste_coherente() {
        let mut a = two_tabs_on_a();
        a.switch_server("B"); // onglet 2 sur B
        a.open_in_new_tab(); // onglet 3 : copie, sur B
        assert_eq!(a.server_name, "B");
        a.switch_tab(-2); // onglet 1 (A)
        assert_eq!(a.server_name, "A");
        a.close_tab(); // ferme l'onglet 1 : on retombe sur un onglet de B
        assert_eq!(a.server_name, "B");
        assert_eq!(a.tab_count(), 2);
        assert!(a.tab_titles().iter().all(|t| t.server == "B"),);
    }

    #[test]
    fn selecteur_de_serveur() {
        let mut a = fresh();
        a.open_server_picker();
        assert_eq!(
            a.server_picker,
            Some(0),
            "la liste s'ouvre même sans serveur : on peut en ajouter"
        );
        a.close_server_picker();
        a.set_servers(vec![entry("A"), entry("B"), entry("C")]);
        a.set_initial_server("A");
        a.set_caps("A", true, true);
        a.set_caps("B", true, true);
        a.open_server_picker();
        assert_eq!(a.server_picker, Some(0), "commence sur le serveur courant");
        a.server_picker_move(1);
        // B est connecté : bascule directe
        assert_eq!(a.server_picker_choose(), None);
        assert_eq!((a.server_name.as_str(), a.server_picker), ("B", None));
        // C ne l'est pas : le choix est renvoyé pour ouvrir le login, rien ne change encore
        a.open_server_picker();
        a.server_picker_move(1);
        assert_eq!(a.server_picker_choose().as_deref(), Some("C"));
        assert_eq!(a.server_name, "B");
        // le serveur déjà affiché : rien
        a.open_server_picker();
        assert_eq!(a.server_picker_choose(), None);
    }

    #[test]
    fn la_connexion_d_une_bascule_ne_change_l_onglet_qu_une_fois_reussie() {
        let mut a = fresh();
        a.set_servers(vec![entry("A"), entry("B")]);
        a.set_initial_server("A");
        a.set_caps("A", true, true);
        a.open_login("B", "thomas", true);
        assert_eq!(a.login.as_ref().unwrap().label, "B (http://B)");
        // Un échec ne bascule pas
        a.login_enter();
        a.apply_for(
            "B",
            Msg::LoggedIn {
                server: "B".into(),
                result: Err("non".into()),
            },
        );
        assert_eq!(a.server_name, "A");
        assert!(a.login.is_some());
        // Succès d'un autre serveur : ignoré
        let session = Session {
            url: "http://B".into(),
            username: "thomas".into(),
            token: None,
            cookies: Default::default(),
        };
        a.apply_for(
            "C",
            Msg::LoggedIn {
                server: "C".into(),
                result: Ok(session.clone()),
            },
        );
        assert!(a.login.is_some() && a.server_name == "A");
        // Succès : main annonce les capacités, puis l'onglet bascule
        a.set_caps("B", true, true);
        a.apply_for(
            "B",
            Msg::LoggedIn {
                server: "B".into(),
                result: Ok(session),
            },
        );
        assert!(a.login.is_none());
        assert_eq!(a.server_name, "B");
    }

    #[test]
    fn echap_dans_le_login() {
        let mut a = fresh();
        a.set_servers(vec![entry("A"), entry("B")]);
        a.set_initial_server("A");
        // Connexion de démarrage (A pas connecté) : quitte
        a.caps.clear();
        a.open_login("A", "", false);
        assert!(a.login_escape());
        assert!(a.login.is_none());
        // Bascule annulée : ferme, l'onglet reste comme il était
        a.open_login("B", "", true);
        assert!(!a.login_escape());
        assert_eq!(a.server_name, "A");
        // Renouvellement de session d'un serveur connecté : ferme
        a.set_caps("A", true, true);
        // Serveur courant déconnecté (URL modifiée) mais un autre est connecté : ferme
        a.set_caps("B", true, true);
        a.forget_caps("A");
        a.open_login("A", "t", false);
        assert!(!a.login_escape());
        a.set_caps("A", true, true);
        a.open_login("A", "t", false);
        assert!(!a.login_escape());
        assert!(!a.login_escape(), "sans formulaire : rien");
    }

    // --- ajout, modification, suppression

    fn type_str(a: &mut App, text: &str) {
        text.chars().for_each(|c| a.server_form_type(c));
    }

    #[test]
    fn ajout_d_un_serveur() {
        let mut a = fresh();
        a.set_servers(vec![entry("A")]);
        a.set_initial_server("A");
        a.open_server_picker();
        a.server_form_add();
        let f = a.server_form.as_ref().unwrap();
        assert_eq!((f.field, f.url.as_str()), (ServerField::Name, "https://"));
        // Nom déjà pris : erreur, curseur sur le nom
        type_str(&mut a, "A");
        assert_eq!(a.server_form_enter(), None); // -> URL
        assert_eq!(a.server_form_enter(), None); // -> Token
        assert_eq!(a.server_form_enter(), None); // valide : refusé
        let f = a.server_form.as_ref().unwrap();
        assert!(f.error.as_deref().unwrap().contains("déjà"));
        assert_eq!(f.field, ServerField::Name);
        // Corrige, URL invalide
        a.server_form_backspace();
        type_str(&mut a, "B");
        a.server_form_enter();
        a.server_form_enter();
        assert_eq!(a.server_form_enter(), None); // « https:// » seul
        assert!(a.server_form.as_ref().unwrap().error.is_some());
        assert_eq!(a.server_form.as_ref().unwrap().field, ServerField::Url);
        // URL correcte (la barre finale est retirée), token facultatif
        type_str(&mut a, "x.example/");
        a.server_form_move(1);
        let draft = a.server_form_enter().unwrap();
        assert_eq!(
            draft,
            ServerDraft {
                original: None,
                name: "B".into(),
                url: "https://x.example".into(),
                token: None
            }
        );
        // main enregistre puis annonce la fin : la liste se met à jour, le formulaire se ferme
        a.server_form_done(vec![entry("A"), entry("B")], "B");
        assert!(a.server_form.is_none());
        assert_eq!((a.servers.len(), a.server_picker), (2, Some(1)));
    }

    #[test]
    fn modification_d_un_serveur_nom_fixe_token_vide_inchange() {
        let mut a = fresh();
        a.set_servers(vec![entry("A"), entry("B")]);
        a.set_initial_server("A");
        a.open_server_picker();
        a.server_picker_move(1);
        a.server_form_edit();
        let f = a.server_form.as_ref().unwrap();
        assert_eq!(
            (f.original.as_deref(), f.field),
            (Some("B"), ServerField::Url)
        );
        // Le nom est sauté par Tab
        a.server_form_move(-1);
        assert_eq!(a.server_form.as_ref().unwrap().field, ServerField::Token);
        a.server_form_move(1);
        assert_eq!(a.server_form.as_ref().unwrap().field, ServerField::Url);
        // Son propre nom n'est pas un doublon
        a.server_form_backspace();
        type_str(&mut a, "ZZ");
        a.server_form_move(1);
        let d = a.server_form_enter().unwrap();
        assert_eq!(
            (
                d.original.as_deref(),
                d.name.as_str(),
                d.url.as_str(),
                d.token
            ),
            (Some("B"), "B", "http://ZZ", None)
        );
    }

    #[test]
    fn suppression_refusee_si_utilise_ou_dernier() {
        let mut a = fresh();
        a.set_servers(vec![entry("A"), entry("B"), entry("C")]);
        a.set_initial_server("A");
        a.open_server_picker();
        // A est le serveur d'un onglet
        a.server_delete_ask();
        assert!(a.server_delete.is_none() && a.server_note.as_deref().unwrap().contains("onglet"));
        // B est libre : confirmation demandée, annulable
        a.server_picker_move(1);
        a.server_delete_ask();
        assert_eq!(a.server_delete.as_deref(), Some("B"));
        assert!(a.server_note.is_none());
        a.server_delete_cancel();
        assert!(a.server_delete.is_none());
        a.server_delete_ask();
        assert_eq!(a.server_delete_confirm().as_deref(), Some("B"));
        a.server_delete_done(vec![entry("A"), entry("C")]);
        assert_eq!(a.servers.len(), 2);
        // Un onglet qui n'est pas affiché compte aussi
        a.open_in_new_tab();
        a.switch_server("C");
        a.switch_tab(-1);
        a.set_caps("C", true, true);
        a.server_picker_move(1);
        a.server_delete_ask();
        assert!(a.server_delete.is_none());
        // Le dernier serveur ne se supprime pas
        a.server_delete_done(vec![entry("A")]);
        a.server_picker = Some(0);
        a.server_delete_ask();
        assert!(a.server_note.as_deref().unwrap().contains("dernier"));
    }

    #[test]
    fn oublier_un_serveur_le_deconnecte() {
        let mut a = fresh();
        a.set_servers(vec![entry("A")]);
        a.set_caps("A", true, true);
        assert!(a.is_connected("A"));
        a.forget_caps("A");
        assert!(!a.is_connected("A"));
    }

    // --- joignabilité

    fn reach_msg(n: &str, r: Result<Option<String>, String>) -> Msg {
        Msg::Reach {
            server: n.into(),
            url: format!("http://{n}"),
            result: r,
        }
    }

    #[test]
    fn la_liste_des_serveurs_sonde_chaque_serveur() {
        let mut a = fresh();
        a.set_servers(vec![entry("A"), entry("B")]);
        assert!(
            a.take_probes().is_empty(),
            "rien tant que la liste est fermée"
        );
        a.tick();
        assert!(a.take_probes().is_empty());
        a.open_server_picker();
        assert_eq!(a.reach.get("A"), Some(&Reach::Checking));
        assert_eq!(
            a.take_probes(),
            [
                ("A".into(), "http://A".into()),
                ("B".into(), "http://B".into())
            ]
        );
        // Liste ouverte : chaque tick remesure (le VPN peut revenir), sans doublon
        a.tick();
        a.tick();
        assert_eq!(a.take_probes().len(), 2);
        a.apply_for("A", reach_msg("A", Ok(Some("15.0".into()))));
        a.apply_for("B", reach_msg("B", Err("connexion impossible".into())));
        assert_eq!(a.reach["A"], Reach::Up(Some("15.0".into())));
        assert_eq!(a.reach["B"], Reach::Down("connexion impossible".into()));
        // Le résultat précédent reste affiché pendant la remesure
        a.close_server_picker();
        a.open_server_picker();
        assert_eq!(a.reach["B"], Reach::Down("connexion impossible".into()));
    }

    #[test]
    fn sonde_d_une_ancienne_url_ignoree_et_modification_remesuree() {
        let mut a = fresh();
        a.set_servers(vec![entry("A")]);
        a.apply_for(
            "A",
            Msg::Reach {
                server: "A".into(),
                url: "http://ancienne".into(),
                result: Ok(None),
            },
        );
        assert!(a.reach.is_empty());
        a.server_form_done(
            vec![ServerEntry {
                name: "A".into(),
                url: "http://nouvelle".into(),
            }],
            "A",
        );
        assert_eq!(a.reach["A"], Reach::Checking);
        assert_eq!(a.take_probes(), [("A".into(), "http://nouvelle".into())]);
        // Un serveur supprimé n'a plus d'état
        a.server_delete_done(vec![]);
        assert!(a.reach.is_empty());
    }

    // --- sauvegarde et restauration des onglets

    fn saved(server: &str, pages: Vec<SavedPage>) -> SavedTab {
        SavedTab {
            server: server.into(),
            pages,
        }
    }

    fn home_page(section: usize, selected: usize) -> SavedPage {
        SavedPage::Home { section, selected }
    }

    #[test]
    fn instantane_des_onglets() {
        let mut a = two_tabs_on_a(); // onglet 2 = copie des runs, ouvert sur les logs
        a.switch_server("B");
        a.switch_tab(-1);
        let snap = a.snapshot();
        assert_eq!(snap.active, 0);
        assert_eq!(snap.tabs.len(), 2);
        assert_eq!(snap.tabs[0].server, "A");
        assert_eq!(
            snap.tabs[0].pages,
            [
                home_page(0, 0),
                SavedPage::Repo {
                    repo: repo("a"),
                    selected: 0
                },
                SavedPage::Runs { repo: repo("a") },
            ]
        );
        assert_eq!(snap.tabs[1], saved("B", vec![home_page(0, 0)]));
        // Les logs ne sont pas gardés : l'onglet revient à la liste des runs
        let mut b = app_on_run(Status::Running);
        b.open_logs();
        let pages = &b.snapshot().tabs[0].pages;
        assert!(
            matches!(pages.last(), Some(SavedPage::Runs { .. })),
            "{pages:?}"
        );
    }

    #[test]
    fn restauration_des_onglets_avec_leurs_requetes() {
        let mut a = fresh();
        a.set_servers(vec![entry("A"), entry("B")]);
        a.set_caps("A", true, true);
        a.set_caps("B", true, true);
        let r = repo("a");
        let saved = SavedTabs::new(
            1,
            vec![
                saved(
                    "A",
                    vec![
                        home_page(2, 4),
                        SavedPage::Repo {
                            repo: r.clone(),
                            selected: 1,
                        },
                    ],
                ),
                saved(
                    "B",
                    vec![
                        home_page(0, 0),
                        SavedPage::Repo {
                            repo: r.clone(),
                            selected: 0,
                        },
                        SavedPage::Browser {
                            repo: r.clone(),
                            reference: "feature/x".into(),
                            path: "src".into(),
                        },
                    ],
                ),
            ],
        );
        assert!(a.restore(saved.clone()));
        assert_eq!(
            (a.tab_count(), active_index(&a), a.server_name.as_str()),
            (2, 1, "B")
        );
        let b = a.browser().unwrap();
        assert_eq!(
            (b.reference.as_str(), b.path.as_str()),
            ("feature/x", "src")
        );
        // Un instantané refait donne la même chose (aller-retour)
        assert_eq!(a.snapshot(), saved);
        // Chaque requête part vers le serveur de son onglet
        let reqs = a.take_tagged_requests();
        let for_server = |s: &str| {
            reqs.iter()
                .filter(|(n, _)| n == s)
                .map(|(_, r)| r.clone())
                .collect::<Vec<_>>()
        };
        assert!(for_server("B").contains(&Request::RepoInfo(r.clone())));
        assert!(for_server("B").contains(&Request::Branches(r.clone())));
        assert!(for_server("B").iter().any(
            |q| matches!(q, Request::Dir { reference, path, .. } if reference == "feature/x" && path == "src")
        ));
        // L'onglet 1 : section « Étoilés » (la 3e) rechargée, fiche et runs du dépôt
        assert!(
            for_server("A")
                .iter()
                .any(|q| matches!(q, Request::Repos(RepoSource::Starred)))
        );
        assert!(for_server("A").contains(&Request::Pipelines(r)));
        // Les requêtes de l'accueil par défaut ne sont pas parties en plus
        assert!(!reqs.iter().any(|(n, _)| n.is_empty()));
        a.switch_tab(-1);
        // Section et ligne de l'accueil de l'onglet 1 retrouvées sous sa page de dépôt
        let Page::Home(h) = &a.stack[0] else {
            panic!("pas d'accueil au fond");
        };
        assert_eq!((h.section.index(), h.data().selected), (2, 4));
    }

    #[test]
    fn restauration_ecarte_les_serveurs_disparus() {
        let mut a = fresh();
        a.set_servers(vec![entry("A")]);
        a.set_caps("A", true, true);
        let before = a.snapshot();
        // Rien d'exploitable : l'état ne change pas
        assert!(!a.restore(SavedTabs::new(0, vec![saved("Z", vec![home_page(0, 0)])])));
        assert_eq!(a.snapshot(), before);
        // L'onglet du serveur disparu est écarté, l'actif retombe sur le suivant
        let ok = a.restore(SavedTabs::new(
            1,
            vec![
                saved("A", vec![home_page(1, 0)]),
                saved("Z", vec![home_page(0, 0)]),
                saved("A", vec![home_page(3, 0)]),
            ],
        ));
        assert!(ok);
        assert_eq!((a.tab_count(), active_index(&a)), (2, 1));
        assert_eq!(a.home().unwrap().section.index(), 3);
        // Pile sans accueil, section hors bornes : réparées
        let ok = a.restore(SavedTabs::new(
            0,
            vec![saved(
                "A",
                vec![SavedPage::Runs { repo: repo("a") }, home_page(99, 0)],
            )],
        ));
        assert!(ok);
        assert!(matches!(a.stack[0], Page::Home(_)));
        assert!(a.snapshot().tabs[0].pages.len() >= 2);
    }

    // --- coloration syntaxique

    fn open_file(a: &mut App, path: &str, text: &str) {
        feed(a);
        {
            let b = a.browser_mut().unwrap();
            b.move_selection(1); // « a.txt » (dossiers d'abord)
            b.enter();
        }
        // Le fichier ouvert porte le nom demandé
        if let Mode::File(fv) = &mut a.browser_mut().unwrap().mode {
            fv.path = path.to_owned();
        }
        a.apply(Msg::File {
            repo: repo("a"),
            reference: "main".into(),
            path: path.into(),
            result: Ok(FileContent {
                size: text.len() as u64,
                text: Some(text.into()),
                truncated: false,
            }),
        });
    }

    fn file_runs(a: &App) -> Vec<usize> {
        let Mode::File(fv) = &a.browser().unwrap().mode else {
            panic!("pas de fichier ouvert");
        };
        let crate::logview::LogState::Ready(lines) = &fv.text.state else {
            panic!("fichier pas prêt");
        };
        lines.iter().map(|l| l.runs.len()).collect()
    }

    #[test]
    fn la_coloration_arrive_apres_le_texte_et_seulement_pour_le_meme_texte() {
        let mut a = explorer();
        open_file(&mut a, "a.txt", "let a = 1;\nlet b = 2;\n");
        assert_eq!(file_runs(&a), [0, 0], "d'abord le texte, sans couleur");
        let lines = crate::browser::file_lines("let a = 1;\nlet b = 2;\n").0;
        let fingerprint = crate::logview::fingerprint_of(&lines);
        let run = |s, e| Run {
            start: s,
            end: e,
            tok: crate::highlight::Tok::Keyword,
        };
        let styles = vec![vec![run(0, 3)], vec![run(0, 3)]];
        let styled = |fingerprint, path: &str, styles| Msg::FileStyles {
            repo: repo("a"),
            reference: "main".into(),
            path: path.into(),
            fingerprint,
            styles,
        };
        // Un autre fichier, un autre dépôt ou un texte différent : ignoré
        a.apply(styled(fingerprint, "autre.rs", styles.clone()));
        a.apply(styled(fingerprint ^ 1, "a.txt", styles.clone()));
        assert_eq!(file_runs(&a), [0, 0]);
        // Un nombre de lignes différent : ignoré
        a.apply(styled(fingerprint, "a.txt", vec![vec![run(0, 3)]]));
        assert_eq!(file_runs(&a), [0, 0]);
        // Le bon : appliqué
        a.apply(styled(fingerprint, "a.txt", styles));
        assert_eq!(file_runs(&a), [1, 1]);
        // Le fichier est rechargé avec un autre texte : l'ancienne coloration est partie
        a.apply(Msg::File {
            repo: repo("a"),
            reference: "main".into(),
            path: "a.txt".into(),
            result: Ok(FileContent {
                size: 5,
                text: Some("autre\nchose\n".into()),
                truncated: false,
            }),
        });
        assert_eq!(file_runs(&a), [0, 0]);
    }

    #[test]
    fn lignes_du_fichier_affichees_et_analysees_sont_les_memes() {
        // Tabulation et contrôle : le texte analysé est celui qui est à l'écran
        let (lines, cut) = crate::browser::file_lines("a\tb\x01\nc\r\n");
        assert!(!cut);
        assert_eq!(lines, ["a    b\u{2401}", "c"]);
        assert!(crate::browser::file_lines("").0.is_empty());
    }
}
