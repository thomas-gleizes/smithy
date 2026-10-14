//! États des pages de navigation : l'accueil (listes de dépôts), la page d'un dépôt et la liste
//! des runs de sa CI. Purs, sans terminal ni réseau : ils déposent des `Request` que la boucle
//! principale exécute, et ignorent les réponses devenues périmées.

use std::cell::Cell;

use smithy_core::{Error, Pipeline, Repo, RepoInfo, RepoSource};

use crate::{recent::RecentEntry, request::Request};

// ---------------------------------------------------------------------------------------------
// Accueil

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Mine,
    Watched,
    Starred,
    Explore,
    Recent,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Mine,
        Section::Watched,
        Section::Starred,
        Section::Explore,
        Section::Recent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Mine => "Mes dépôts",
            Section::Watched => "Suivis",
            Section::Starred => "Étoilés",
            Section::Explore => "Explorer",
            Section::Recent => "Récents",
        }
    }

    pub fn index(self) -> usize {
        Section::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// D'où viennent ses dépôts, quand c'est une simple liste à charger.
    fn source(self) -> Option<RepoSource> {
        match self {
            Section::Mine => Some(RepoSource::Mine),
            Section::Watched => Some(RepoSource::Watched),
            Section::Starred => Some(RepoSource::Starred),
            Section::Explore | Section::Recent => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Load {
    /// Pas encore demandée (ou, pour « Explorer », aucune recherche).
    Idle,
    Loading,
    Ready(Vec<RepoInfo>),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct SectionData {
    pub load: Load,
    pub selected: usize,
    /// Filtre local (listes) ou recherche en cours de saisie (« Explorer »).
    pub filter: String,
}

/// Une ligne affichée : un dépôt, avec ce qu'on en sait selon la section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeRow {
    pub repo: Repo,
    pub info: Option<RepoInfo>,
    /// Pour « Récents » : quand on l'a ouvert (secondes depuis 1970).
    pub opened: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct HomeView {
    pub section: Section,
    data: [SectionData; 5],
    /// Saisie du filtre ou de la recherche en cours.
    pub editing: bool,
    /// Dernière recherche envoyée : les réponses d'une autre sont ignorées.
    submitted: String,
    /// Décalage de la liste, mémorisé par le rendu pour un défilement stable.
    pub list_offset: Cell<usize>,
}

impl HomeView {
    pub fn new() -> Self {
        let section = || SectionData {
            load: Load::Idle,
            selected: 0,
            filter: String::new(),
        };
        Self {
            section: Section::Mine,
            data: [section(), section(), section(), section(), section()],
            editing: false,
            submitted: String::new(),
            list_offset: Cell::new(0),
        }
    }

    pub fn data(&self) -> &SectionData {
        &self.data[self.section.index()]
    }

    fn data_mut(&mut self) -> &mut SectionData {
        &mut self.data[self.section.index()]
    }

    /// La recherche envoyée pour « Explorer » (vide : aucune).
    pub fn submitted(&self) -> &str {
        &self.submitted
    }

    /// Charge la section courante si elle ne l'est pas encore.
    pub fn request_current(&mut self) -> Option<Request> {
        let source = self.section.source()?;
        let d = self.data_mut();
        if matches!(d.load, Load::Idle) {
            d.load = Load::Loading;
            return Some(Request::Repos(source));
        }
        None
    }

    /// Section suivante (`1`) ou précédente (`-1`), bornée aux extrémités.
    pub fn switch(&mut self, delta: isize) -> Option<Request> {
        let i = self
            .section
            .index()
            .saturating_add_signed(delta)
            .min(Section::ALL.len() - 1);
        self.section = Section::ALL[i];
        self.editing = false;
        self.list_offset.set(0);
        self.request_current()
    }

    /// Touche `r` : recharge la section courante.
    pub fn refresh(&mut self) -> Option<Request> {
        if let Some(source) = self.section.source() {
            self.data_mut().load = Load::Loading;
            return Some(Request::Repos(source));
        }
        if self.section == Section::Explore && !self.submitted.is_empty() {
            let q = self.submitted.clone();
            self.data_mut().load = Load::Loading;
            return Some(Request::Repos(RepoSource::Search(q)));
        }
        None
    }

    /// Les lignes de la section courante, filtre appliqué.
    pub fn rows(&self, recents: &[RecentEntry], server: &str) -> Vec<HomeRow> {
        let d = self.data();
        let filter = d.filter.to_lowercase();
        let keep = |text: &str| filter.is_empty() || text.to_lowercase().contains(&filter);
        match self.section {
            Section::Recent => recents
                .iter()
                .filter(|e| e.server == server && keep(&format!("{}/{}", e.owner, e.name)))
                .map(|e| HomeRow {
                    repo: Repo {
                        owner: e.owner.clone(),
                        name: e.name.clone(),
                    },
                    info: None,
                    opened: Some(e.opened),
                })
                .collect(),
            section => match &d.load {
                Load::Ready(list) => list
                    .iter()
                    // « Explorer » : le serveur a déjà filtré ; le texte saisi est la requête.
                    .filter(|i| {
                        section == Section::Explore
                            || keep(&format!("{} {}", i.repo.full_name(), i.description))
                    })
                    .map(|i| HomeRow {
                        repo: i.repo.clone(),
                        info: Some(i.clone()),
                        opened: None,
                    })
                    .collect(),
                _ => Vec::new(),
            },
        }
    }

    /// Indice sélectionné, ramené dans les bornes d'une liste de `len` lignes.
    pub fn selected(&self, len: usize) -> usize {
        self.data().selected.min(len.saturating_sub(1))
    }

    pub fn move_selection(&mut self, delta: isize, len: usize) {
        let max = len.saturating_sub(1);
        let d = self.data_mut();
        d.selected = d.selected.min(max).saturating_add_signed(delta).min(max);
    }

    // --- saisie du filtre / de la recherche

    pub fn start_edit(&mut self) {
        self.editing = true;
    }

    pub fn type_char(&mut self, c: char) {
        if self.editing {
            let d = self.data_mut();
            d.filter.push(c);
            d.selected = 0;
        }
    }

    pub fn backspace(&mut self) {
        if self.editing {
            let d = self.data_mut();
            d.filter.pop();
            d.selected = 0;
        }
    }

    /// Entrée dans la saisie : « Explorer » lance la recherche, les listes se filtrent déjà au fil de la frappe.
    pub fn commit(&mut self) -> Option<Request> {
        self.editing = false;
        if self.section != Section::Explore {
            return None;
        }
        let query = self.data().filter.trim().to_owned();
        let d = self.data_mut();
        d.selected = 0;
        if query.is_empty() {
            d.load = Load::Idle;
            self.submitted.clear();
            return None;
        }
        d.load = Load::Loading;
        self.submitted = query.clone();
        Some(Request::Repos(RepoSource::Search(query)))
    }

    /// Échap pendant la saisie : on l'abandonne.
    pub fn cancel_edit(&mut self) {
        self.editing = false;
        let submitted = self.submitted.clone();
        let explore = self.section == Section::Explore;
        let d = self.data_mut();
        // « Explorer » retrouve la recherche affichée ; une liste perd son filtre.
        d.filter = if explore { submitted } else { String::new() };
        d.selected = 0;
    }

    /// Échap hors saisie : retire le filtre ou la recherche. `true` si quelque chose a bougé.
    pub fn clear_filter(&mut self) -> bool {
        let explore = self.section == Section::Explore;
        if self.data().filter.is_empty() && !(explore && !self.submitted.is_empty()) {
            return false;
        }
        let d = self.data_mut();
        d.filter.clear();
        d.selected = 0;
        if explore {
            d.load = Load::Idle;
            self.submitted.clear();
        }
        true
    }

    /// Une liste arrive. Les réponses à une recherche abandonnée sont ignorées.
    pub fn apply(&mut self, source: &RepoSource, result: Result<Vec<RepoInfo>, Error>) {
        let section = match source {
            RepoSource::Mine => Section::Mine,
            RepoSource::Watched => Section::Watched,
            RepoSource::Starred => Section::Starred,
            RepoSource::Search(q) => {
                if *q != self.submitted {
                    return;
                }
                Section::Explore
            }
        };
        let d = &mut self.data[section.index()];
        d.load = match result {
            Ok(list) => Load::Ready(list),
            Err(e) => Load::Failed(e.to_string()),
        };
    }
}

impl Default for HomeView {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// Page d'un dépôt

#[derive(Debug, Clone)]
pub enum LastRun {
    Loading,
    /// Aucun run.
    None,
    Ready(Pipeline),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct RepoView {
    pub info: RepoInfo,
    pub last_run: LastRun,
    /// Entrée du menu : 0 = CI, 1 = Code.
    pub selected: usize,
    pub info_error: Option<String>,
}

impl RepoView {
    pub const ITEMS: [&'static str; 2] = ["CI", "Code"];

    pub fn new(info: RepoInfo) -> Self {
        Self {
            info,
            last_run: LastRun::Loading,
            selected: 0,
            info_error: None,
        }
    }

    /// Fiche vide, en attendant le serveur (ouverture depuis « Récents »).
    pub fn placeholder(repo: Repo) -> Self {
        Self::new(RepoInfo {
            repo,
            description: String::new(),
            private: false,
            fork: false,
            archived: false,
            empty: false,
            stars: 0,
            open_prs: 0,
            open_issues: 0,
            updated: None,
            default_branch: String::new(),
        })
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(Self::ITEMS.len() - 1);
    }

    pub fn apply_info(&mut self, repo: &Repo, result: Result<RepoInfo, Error>) {
        if *repo != self.info.repo {
            return;
        }
        match result {
            Ok(info) => {
                self.info = info;
                self.info_error = None;
            }
            Err(e) => self.info_error = Some(e.to_string()),
        }
    }

    pub fn apply_pipelines(&mut self, repo: &Repo, result: &Result<Vec<Pipeline>, String>) {
        if *repo != self.info.repo {
            return;
        }
        self.last_run = match result {
            Ok(list) => list.first().cloned().map_or(LastRun::None, LastRun::Ready),
            Err(e) => LastRun::Failed(e.clone()),
        };
    }
}

// ---------------------------------------------------------------------------------------------
// Runs de la CI

#[derive(Debug, Clone)]
pub struct RunsView {
    pub repo: Repo,
    pub pipelines: Vec<Pipeline>,
    pub selected: usize,
    /// Premier chargement en cours (la liste est vide en attendant).
    pub loading: bool,
    pub error: Option<String>,
    pub list_offset: Cell<usize>,
}

impl RunsView {
    pub fn new(repo: Repo) -> Self {
        Self {
            repo,
            pipelines: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
            list_offset: Cell::new(0),
        }
    }

    pub fn selected_pipeline(&self) -> Option<&Pipeline> {
        self.pipelines.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let max = self.pipelines.len().saturating_sub(1);
        self.selected = self.selected.min(max).saturating_add_signed(delta).min(max);
    }

    /// Une liste arrive. La sélection suit le même run même si de nouveaux arrivent en tête.
    pub fn apply_pipelines(&mut self, repo: &Repo, result: Result<Vec<Pipeline>, String>) {
        if *repo != self.repo {
            return;
        }
        self.loading = false;
        match result {
            Ok(list) => {
                let keep = self.selected_pipeline().map(|p| p.id);
                self.pipelines = list;
                self.error = None;
                self.selected = keep
                    .and_then(|id| self.pipelines.iter().position(|p| p.id == id))
                    .unwrap_or_else(|| self.selected.min(self.pipelines.len().saturating_sub(1)));
            }
            Err(e) => self.error = Some(e),
        }
    }
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

    fn info(n: &str, desc: &str) -> RepoInfo {
        let mut i = RepoView::placeholder(repo(n)).info;
        i.description = desc.into();
        i
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

    fn recent(server: &str, name: &str, opened: i64) -> RecentEntry {
        RecentEntry {
            server: server.into(),
            owner: "o".into(),
            name: name.into(),
            opened,
        }
    }

    fn loaded() -> HomeView {
        let mut h = HomeView::new();
        h.request_current();
        h.apply(
            &RepoSource::Mine,
            Ok(vec![
                info("alpha", "le premier"),
                info("beta", "Outil CI"),
                info("gamma", ""),
            ]),
        );
        h
    }

    // --- accueil

    #[test]
    fn chargement_paresseux_des_sections() {
        let mut h = HomeView::new();
        assert_eq!(h.request_current(), Some(Request::Repos(RepoSource::Mine)));
        assert!(matches!(h.data().load, Load::Loading));
        assert_eq!(h.request_current(), None, "déjà demandée : pas de doublon");
        assert_eq!(h.switch(1), Some(Request::Repos(RepoSource::Watched)));
        assert_eq!(h.switch(1), Some(Request::Repos(RepoSource::Starred)));
        assert_eq!(h.switch(1), None, "« Explorer » attend une recherche");
        assert_eq!(h.switch(1), None, "« Récents » est local");
        assert_eq!(h.section, Section::Recent);
        assert_eq!(h.switch(1), None);
        assert_eq!(h.section, Section::Recent, "borné à la dernière");
        for _ in 0..10 {
            h.switch(-1);
        }
        assert_eq!(h.section, Section::Mine, "borné à la première");
        assert_eq!(
            h.switch(1),
            None,
            "« Suivis » est déjà en cours de chargement"
        );
    }

    #[test]
    fn sections_et_libelles() {
        assert_eq!(
            Section::ALL.map(Section::label),
            ["Mes dépôts", "Suivis", "Étoilés", "Explorer", "Récents"]
        );
        assert_eq!(Section::Recent.index(), 4);
    }

    #[test]
    fn lignes_et_filtre_local() {
        let mut h = loaded();
        let rows = |h: &HomeView| {
            h.rows(&[], "s")
                .iter()
                .map(|r| r.repo.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(&h), ["alpha", "beta", "gamma"]);
        h.start_edit();
        "CI".chars().for_each(|c| h.type_char(c)); // casse ignorée, cherche aussi la description
        assert_eq!(rows(&h), ["beta"]);
        h.backspace();
        h.backspace();
        h.type_char('a'); // « a » : alpha, beta, gamma
        assert_eq!(rows(&h), ["alpha", "beta", "gamma"]);
        h.type_char('l'); // « al »
        assert_eq!(rows(&h), ["alpha"]);
        assert!(h.rows(&[], "s")[0].info.is_some());
        // Hors saisie, les caractères ne tapent rien
        h.commit();
        h.type_char('z');
        assert_eq!(h.data().filter, "al");
    }

    #[test]
    fn echap_retire_le_filtre() {
        let mut h = loaded();
        assert!(!h.clear_filter(), "rien à retirer");
        h.start_edit();
        h.type_char('b');
        h.cancel_edit(); // abandon de la saisie : le filtre part avec
        assert!(!h.editing && h.data().filter.is_empty());
        h.start_edit();
        h.type_char('b');
        assert!(h.commit().is_none(), "une liste se filtre sans requête");
        assert_eq!(h.rows(&[], "s").len(), 1);
        assert!(h.clear_filter());
        assert_eq!(h.rows(&[], "s").len(), 3);
    }

    #[test]
    fn recherche_explorer() {
        let mut h = HomeView::new();
        h.section = Section::Explore;
        h.start_edit();
        "tree".chars().for_each(|c| h.type_char(c));
        assert!(
            h.rows(&[], "s").is_empty(),
            "rien tant que rien n'est envoyé"
        );
        let req = h.commit();
        assert_eq!(req, Some(Request::Repos(RepoSource::Search("tree".into()))));
        assert!(matches!(h.data().load, Load::Loading));
        h.apply(
            &RepoSource::Search("tree".into()),
            Ok(vec![info("tree", "x")]),
        );
        assert_eq!(
            h.rows(&[], "s").len(),
            1,
            "résultats non refiltrés localement"
        );
        // Une réponse à une ancienne recherche est ignorée
        h.start_edit();
        h.backspace();
        h.backspace();
        h.commit(); // « tr »
        h.apply(
            &RepoSource::Search("tree".into()),
            Ok(vec![info("perime", "")]),
        );
        assert!(
            matches!(h.data().load, Load::Loading),
            "toujours en attente de « tr »"
        );
        // Rafraîchir relance la recherche affichée
        h.apply(&RepoSource::Search("tr".into()), Ok(vec![]));
        assert_eq!(
            h.refresh(),
            Some(Request::Repos(RepoSource::Search("tr".into())))
        );
        // Échap hors saisie : retire la recherche
        assert!(h.clear_filter());
        assert!(matches!(h.data().load, Load::Idle) && h.submitted().is_empty());
        assert_eq!(h.refresh(), None);
    }

    #[test]
    fn recherche_vide_et_annulation() {
        let mut h = HomeView::new();
        h.section = Section::Explore;
        h.start_edit();
        h.type_char(' ');
        assert_eq!(h.commit(), None, "espaces seuls : pas de requête");
        assert!(matches!(h.data().load, Load::Idle));
        h.start_edit();
        h.type_char('x');
        h.commit();
        h.start_edit();
        h.type_char('y'); // « xy » en cours de saisie
        h.cancel_edit();
        assert_eq!(h.data().filter, "x", "on retrouve la recherche affichée");
    }

    #[test]
    fn recents_par_serveur() {
        let mut h = HomeView::new();
        h.section = Section::Recent;
        let recents = vec![
            recent("a", "un", 30),
            recent("b", "deux", 20),
            recent("a", "trois", 10),
        ];
        let rows = h.rows(&recents, "a");
        assert_eq!(
            rows.iter()
                .map(|r| r.repo.name.as_str())
                .collect::<Vec<_>>(),
            ["un", "trois"]
        );
        assert_eq!(rows[0].opened, Some(30));
        assert!(rows[0].info.is_none());
        h.start_edit();
        h.type_char('t');
        assert_eq!(
            h.rows(&recents, "a").len(),
            1,
            "le filtre s'applique aux récents"
        );
        assert!(h.rows(&recents, "inconnu").is_empty());
    }

    #[test]
    fn echec_de_chargement_et_selection_bornee() {
        let mut h = HomeView::new();
        h.request_current();
        h.apply(&RepoSource::Mine, Err(Error::Network("coupé".into())));
        assert!(matches!(&h.data().load, Load::Failed(m) if m.contains("coupé")));
        assert!(h.rows(&[], "s").is_empty());
        assert_eq!(h.selected(0), 0);
        let mut h = loaded();
        h.move_selection(10, 3);
        assert_eq!(h.selected(3), 2);
        h.move_selection(-10, 3);
        assert_eq!(h.selected(3), 0);
        h.move_selection(2, 3);
        assert_eq!(
            h.selected(1),
            0,
            "la liste a rétréci (filtre) : ramenée dans les bornes"
        );
        h.move_selection(1, 0);
        assert_eq!(h.selected(0), 0);
        assert_eq!(h.refresh(), Some(Request::Repos(RepoSource::Mine)));
        assert!(matches!(h.data().load, Load::Loading));
    }

    #[test]
    fn la_selection_est_par_section() {
        let mut h = loaded();
        h.move_selection(2, 3);
        h.switch(1);
        assert_eq!(h.data().selected, 0);
        h.switch(-1);
        assert_eq!(h.selected(3), 2, "retrouvée en revenant");
    }

    // --- page d'un dépôt

    #[test]
    fn page_de_depot() {
        let mut r = RepoView::new(info("a", "d"));
        assert!(matches!(r.last_run, LastRun::Loading));
        r.move_selection(5);
        assert_eq!(r.selected, 1);
        r.move_selection(-5);
        assert_eq!(r.selected, 0);
        r.apply_pipelines(&repo("a"), &Ok(vec![pipe(9), pipe(8)]));
        assert!(matches!(&r.last_run, LastRun::Ready(p) if p.id == 9));
        r.apply_pipelines(&repo("a"), &Ok(vec![]));
        assert!(matches!(r.last_run, LastRun::None));
        r.apply_pipelines(&repo("a"), &Err("coupé".into()));
        assert!(matches!(&r.last_run, LastRun::Failed(m) if m == "coupé"));
        // Un autre dépôt : ignoré
        r.apply_pipelines(&repo("b"), &Ok(vec![pipe(1)]));
        assert!(matches!(r.last_run, LastRun::Failed(_)));
        let mut newer = info("a", "nouvelle description");
        newer.open_prs = 4;
        r.apply_info(&repo("a"), Ok(newer));
        assert_eq!(
            (r.info.description.as_str(), r.info.open_prs),
            ("nouvelle description", 4)
        );
        r.apply_info(&repo("b"), Ok(info("b", "autre")));
        assert_eq!(r.info.repo.name, "a");
        r.apply_info(&repo("a"), Err(Error::NotFound("a".into())));
        assert!(r.info_error.is_some());
        assert_eq!(r.info.open_prs, 4, "la fiche déjà connue reste");
    }

    // --- runs

    #[test]
    fn runs_selection_et_rafraichissement() {
        let mut r = RunsView::new(repo("a"));
        assert!(r.loading && r.selected_pipeline().is_none());
        r.move_selection(3); // liste vide : sans effet
        r.apply_pipelines(&repo("a"), Ok(vec![pipe(3), pipe(2), pipe(1)]));
        assert!(!r.loading);
        r.move_selection(1);
        assert_eq!(r.selected_pipeline().unwrap().id, 2);
        // De nouveaux runs arrivent en tête : la sélection suit « son » run
        r.apply_pipelines(
            &repo("a"),
            Ok(vec![pipe(5), pipe(4), pipe(3), pipe(2), pipe(1)]),
        );
        assert_eq!(r.selected_pipeline().unwrap().id, 2);
        // Le run sélectionné disparaît : on reste dans les bornes
        r.move_selection(10);
        r.apply_pipelines(&repo("a"), Ok(vec![pipe(9)]));
        assert_eq!(r.selected, 0);
        // Autre dépôt ignoré ; erreur gardée, liste conservée
        r.apply_pipelines(&repo("b"), Ok(vec![]));
        assert_eq!(r.pipelines.len(), 1);
        r.apply_pipelines(&repo("a"), Err("coupé".into()));
        assert_eq!((r.error.as_deref(), r.pipelines.len()), (Some("coupé"), 1));
        r.apply_pipelines(&repo("a"), Ok(vec![pipe(9)]));
        assert_eq!(r.error, None);
    }
}
