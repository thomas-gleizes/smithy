//! Explorateur de code : état pur (aucune dépendance au terminal ni au réseau). Les actions de
//! l'utilisateur et les réponses du backend font évoluer `Browser`, qui dépose les requêtes à
//! lancer dans une file (`take_requests`) ; la boucle principale les exécute. Les réponses
//! périmées (autre branche, autre dossier, autre fichier) sont ignorées.

use std::cell::Cell;

use smithy_core::{Branch, Entry, EntryKind, Error, FileContent, Repo, clean_text_line};

use crate::logview::TextView;

/// Octets lus d'un fichier au plus : au-delà, seul le début est affiché.
pub const FILE_LIMIT_BYTES: usize = 1024 * 1024;
/// Lignes affichées au plus (un million de lignes vides tiendrait dans la limite d'octets).
pub const FILE_LIMIT_LINES: usize = 50_000;

/// Ce que la boucle principale doit demander au backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
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

#[derive(Debug)]
pub enum DirState {
    Loading,
    Ready(Vec<Entry>),
    Failed(String),
}

/// Un fichier ouvert, dans un volet de texte (défilement, retour à la ligne).
#[derive(Debug)]
pub struct FileView {
    pub path: String,
    pub text: TextView,
}

#[derive(Debug)]
pub enum Mode {
    Dir,
    /// Sélecteur de branche par-dessus le dossier.
    Branches {
        selected: usize,
    },
    File(FileView),
}

/// L'écran affiché, sans les données : pratique pour choisir les touches à interpréter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Dir,
    Branches,
    File,
}

#[derive(Debug)]
pub struct Browser {
    pub repo: Repo,
    /// Branche affichée ; vide tant que la branche par défaut n'est pas connue.
    pub reference: String,
    /// `None` : pas encore reçues (ou en échec, voir `branches_error`).
    pub branches: Option<Vec<Branch>>,
    pub branches_error: Option<String>,
    /// Dossier courant, sans `/` initial ni final ; vide à la racine.
    pub path: String,
    pub dir: DirState,
    /// Indice de l'entrée sélectionnée dans le dossier trié.
    pub selected: usize,
    pub mode: Mode,
    /// Dépôt sans aucune branche.
    pub empty_repo: bool,
    /// Message court en pied de page (ex. lien symbolique non ouvert).
    pub info: Option<String>,
    /// Décalage de la liste affichée, mémorisé par le rendu pour que le défilement reste stable.
    pub list_offset: Cell<usize>,
    /// Idem pour le sélecteur de branche.
    pub picker_offset: Cell<usize>,
    /// Entrée à resélectionner à l'arrivée du dossier (celui d'où l'on vient en remontant).
    reselect: Option<String>,
    requests: Vec<Request>,
}

impl Browser {
    /// Ouvre l'explorateur sur un dépôt : on demande d'abord les branches pour connaître la
    /// branche par défaut, puis la racine.
    pub fn open(repo: Repo) -> Self {
        Self {
            requests: vec![Request::Branches(repo.clone())],
            repo,
            reference: String::new(),
            branches: None,
            branches_error: None,
            path: String::new(),
            dir: DirState::Loading,
            selected: 0,
            mode: Mode::Dir,
            empty_repo: false,
            info: None,
            list_offset: Cell::new(0),
            picker_offset: Cell::new(0),
            reselect: None,
        }
    }

    /// Fil d'Ariane de la page : dépôt, « Code », branche, puis les dossiers (ou le fichier).
    pub fn breadcrumb(&self) -> Vec<String> {
        let reference = if self.reference.is_empty() {
            "…"
        } else {
            &self.reference
        };
        let path = match &self.mode {
            Mode::File(fv) => fv.path.as_str(),
            _ => self.path.as_str(),
        };
        let mut out = vec![
            self.repo.full_name(),
            "Code".to_owned(),
            reference.to_owned(),
        ];
        out.extend(path.split('/').filter(|s| !s.is_empty()).map(str::to_owned));
        out
    }

    pub fn screen(&self) -> Screen {
        match self.mode {
            Mode::Dir => Screen::Dir,
            Mode::Branches { .. } => Screen::Branches,
            Mode::File(_) => Screen::File,
        }
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    fn request_dir(&mut self) {
        self.dir = DirState::Loading;
        self.requests.push(Request::Dir {
            repo: self.repo.clone(),
            reference: self.reference.clone(),
            path: self.path.clone(),
        });
    }

    pub fn entries(&self) -> &[Entry] {
        match &self.dir {
            DirState::Ready(e) => e,
            _ => &[],
        }
    }

    // --- réponses du backend

    pub fn on_branches(&mut self, repo: &Repo, result: Result<Vec<Branch>, Error>) {
        if *repo != self.repo {
            return;
        }
        match result {
            Ok(list) => {
                self.branches_error = None;
                if self.reference.is_empty() {
                    match list.iter().find(|b| b.default).or(list.first()) {
                        Some(b) => {
                            self.reference = b.name.clone();
                            self.request_dir();
                        }
                        None => {
                            self.empty_repo = true;
                            self.dir = DirState::Ready(Vec::new());
                        }
                    }
                }
                self.branches = Some(list);
            }
            Err(e) if self.reference.is_empty() => self.dir = DirState::Failed(e.to_string()),
            Err(e) => self.branches_error = Some(e.to_string()),
        }
    }

    pub fn on_dir(
        &mut self,
        repo: &Repo,
        reference: &str,
        path: &str,
        result: Result<Vec<Entry>, Error>,
    ) {
        if *repo != self.repo || reference != self.reference || path != self.path {
            return;
        }
        match result {
            Ok(mut entries) => {
                // Dossiers d'abord, puis ordre alphabétique sans tenir compte de la casse.
                entries.sort_by_cached_key(|e| (e.kind != EntryKind::Dir, e.name.to_lowercase()));
                self.selected = self
                    .reselect
                    .take()
                    .and_then(|name| entries.iter().position(|e| e.name == name))
                    .unwrap_or(0);
                // Le décalage mémorisé appartient à l'ancien dossier : le garder laisserait le
                // début d'un dossier court caché au-dessus de la sélection (ratatui ne fait que
                // descendre jusqu'à la sélection, il ne remonte pas pour remplir l'écran).
                self.list_offset.set(0);
                self.dir = DirState::Ready(entries);
            }
            Err(e) => {
                self.reselect = None;
                self.dir = DirState::Failed(e.to_string());
            }
        }
    }

    pub fn on_file(
        &mut self,
        repo: &Repo,
        reference: &str,
        path: &str,
        result: Result<FileContent, Error>,
    ) {
        let Mode::File(fv) = &mut self.mode else {
            return;
        };
        if *repo != self.repo || reference != self.reference || path != fv.path {
            return;
        }
        match result {
            Ok(content) => show_file(&mut fv.text, &content),
            Err(e) => fv.text.state = crate::logview::LogState::Failed(e.to_string()),
        }
    }

    // --- actions de l'utilisateur

    /// Entrée / flèche droite : ouvre le dossier ou le fichier sélectionné, ou choisit la
    /// branche du sélecteur.
    pub fn enter(&mut self) {
        match &self.mode {
            Mode::Branches { selected } => {
                let Some(name) = self
                    .branches
                    .as_ref()
                    .and_then(|b| b.get(*selected))
                    .map(|b| b.name.clone())
                else {
                    return;
                };
                self.reference = name;
                self.path.clear();
                self.selected = 0;
                self.mode = Mode::Dir;
                self.request_dir();
            }
            Mode::Dir => {
                let Some(entry) = self.entries().get(self.selected).cloned() else {
                    return;
                };
                self.info = None;
                match entry.kind {
                    EntryKind::Dir => {
                        self.path = entry.path;
                        self.selected = 0;
                        self.request_dir();
                    }
                    EntryKind::File => {
                        self.mode = Mode::File(FileView {
                            path: entry.path.clone(),
                            text: TextView::new(),
                        });
                        self.request_file(entry.path);
                    }
                    EntryKind::Symlink => {
                        self.info = Some(format!("{} : lien symbolique, non ouvert", entry.name));
                    }
                    EntryKind::Submodule => {
                        self.info = Some(format!("{} : sous-module, non ouvert", entry.name));
                    }
                }
            }
            Mode::File(_) => {}
        }
    }

    fn request_file(&mut self, path: String) {
        self.requests.push(Request::File {
            repo: self.repo.clone(),
            reference: self.reference.clone(),
            path,
        });
    }

    /// Flèche gauche / Retour arrière : remonte d'un dossier (retombe sur celui d'où l'on vient),
    /// ou ferme le fichier. Sans effet à la racine.
    pub fn up(&mut self) {
        match &self.mode {
            Mode::File(_) | Mode::Branches { .. } => self.mode = Mode::Dir,
            Mode::Dir => {
                if self.path.is_empty() {
                    return;
                }
                let (parent, name) = match self.path.rsplit_once('/') {
                    Some((p, n)) => (p.to_owned(), n.to_owned()),
                    None => (String::new(), self.path.clone()),
                };
                self.reselect = Some(name);
                self.path = parent;
                self.info = None;
                self.request_dir();
            }
        }
    }

    /// Échap / q : ferme le fichier ou le sélecteur ; renvoie `true` si c'est l'explorateur
    /// lui-même qu'il faut fermer.
    pub fn back(&mut self) -> bool {
        match self.mode {
            Mode::Dir => true,
            _ => {
                self.mode = Mode::Dir;
                false
            }
        }
    }

    /// Ouvre le sélecteur de branche (et recharge la liste si elle manque).
    pub fn open_branches(&mut self) {
        if !matches!(self.mode, Mode::Dir) || self.empty_repo {
            return;
        }
        let current = self
            .branches
            .as_ref()
            .and_then(|b| b.iter().position(|x| x.name == self.reference))
            .unwrap_or(0);
        self.picker_offset.set(0);
        self.mode = Mode::Branches { selected: current };
        if self.branches.is_none() {
            self.branches_error = None;
            self.requests.push(Request::Branches(self.repo.clone()));
        }
    }

    /// Touche `r` : redemande ce qui est affiché.
    pub fn refresh(&mut self) {
        match &self.mode {
            Mode::Dir if self.reference.is_empty() => {
                self.dir = DirState::Loading;
                self.requests.push(Request::Branches(self.repo.clone()));
            }
            Mode::Dir => self.request_dir(),
            Mode::Branches { .. } => {
                self.branches_error = None;
                self.requests.push(Request::Branches(self.repo.clone()));
            }
            Mode::File(fv) => {
                let path = fv.path.clone();
                self.request_file(path);
            }
        }
    }

    // --- déplacement dans la liste courante (dossier ou sélecteur de branche)

    fn len(&self) -> usize {
        match &self.mode {
            Mode::Branches { .. } => self.branches.as_ref().map_or(0, Vec::len),
            _ => self.entries().len(),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let max = self.len().saturating_sub(1);
        match &mut self.mode {
            Mode::Branches { selected } => {
                *selected = selected.saturating_add_signed(delta).min(max)
            }
            Mode::Dir => self.selected = self.selected.saturating_add_signed(delta).min(max),
            Mode::File(_) => {}
        }
    }

    pub fn select_first(&mut self) {
        self.move_selection(isize::MIN / 2);
    }

    pub fn select_last(&mut self) {
        self.move_selection(isize::MAX / 2);
    }
}

/// Remplit le volet de texte avec le contenu d'un fichier, et dit ce qui a été coupé.
fn show_file(view: &mut TextView, content: &FileContent) {
    let Some(text) = &content.text else {
        view.set_plain(vec![format!(
            "(fichier binaire de {} : non affiché)",
            fmt_size(content.size)
        )]);
        view.notice = None;
        return;
    };
    let mut lines: Vec<&str> = text.split('\n').collect();
    // Un fichier qui finit par un saut de ligne ne compte pas de ligne vide en plus.
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let cut_lines = lines.len() > FILE_LIMIT_LINES;
    lines.truncate(FILE_LIMIT_LINES);
    let shown: Vec<String> = if lines.is_empty() {
        vec!["(fichier vide)".to_owned()]
    } else {
        lines.iter().map(|l| clean_text_line(l)).collect()
    };
    view.set_plain(shown);
    view.notice = match (content.truncated, cut_lines) {
        (true, _) => Some(format!(
            "fichier tronqué : début affiché ({} sur {})",
            fmt_size(text.len() as u64),
            fmt_size(content.size)
        )),
        (false, true) => Some(format!(
            "affichage limité aux {FILE_LIMIT_LINES} premières lignes"
        )),
        _ => None,
    };
}

/// Taille lisible : `12 o`, `1,5 Kio`, `2,0 Mio`.
pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["o", "Kio", "Mio", "Gio"];
    if bytes < 1024 {
        return format!("{bytes} o");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit]).replace('.', ",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logview::LogState;

    fn repo() -> Repo {
        Repo {
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn branch(name: &str, default: bool) -> Branch {
        Branch {
            name: name.into(),
            default,
        }
    }

    fn entry(name: &str, path: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.into(),
            path: path.into(),
            kind,
            size,
        }
    }

    /// Un explorateur ouvert, branches reçues, racine affichée.
    fn browsing() -> Browser {
        let mut b = Browser::open(repo());
        assert_eq!(b.take_requests(), [Request::Branches(repo())]);
        b.on_branches(
            &repo(),
            Ok(vec![branch("main", true), branch("feature/x", false)]),
        );
        assert_eq!(
            b.take_requests(),
            [Request::Dir {
                repo: repo(),
                reference: "main".into(),
                path: String::new()
            }]
        );
        b.on_dir(
            &repo(),
            "main",
            "",
            Ok(vec![
                entry("src", "src", EntryKind::Dir, 0),
                entry("README.md", "README.md", EntryKind::File, 10),
                entry("Zebra.txt", "Zebra.txt", EntryKind::File, 1),
                entry("lien", "lien", EntryKind::Symlink, 4),
                entry("alpha", "alpha", EntryKind::Dir, 0),
                entry("sub", "sub", EntryKind::Submodule, 0),
            ]),
        );
        b
    }

    fn names(b: &Browser) -> Vec<&str> {
        b.entries().iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn ouverture_branche_par_defaut_puis_racine() {
        let b = browsing();
        assert_eq!(b.reference, "main");
        assert!(matches!(b.dir, DirState::Ready(_)));
        // Dossiers d'abord, alphabétique sans casse
        assert_eq!(
            names(&b),
            ["alpha", "src", "lien", "README.md", "sub", "Zebra.txt"]
        );
        assert_eq!(b.selected, 0);
    }

    #[test]
    fn premiere_branche_si_aucune_par_defaut_et_depot_vide() {
        let mut b = Browser::open(repo());
        b.take_requests();
        b.on_branches(
            &repo(),
            Ok(vec![branch("zeta", false), branch("alpha", false)]),
        );
        assert_eq!(
            b.reference, "zeta",
            "sans branche par défaut : la première de la liste"
        );

        let mut v = Browser::open(repo());
        v.take_requests();
        v.on_branches(&repo(), Ok(vec![]));
        assert!(v.empty_repo);
        assert!(matches!(&v.dir, DirState::Ready(e) if e.is_empty()));
        assert!(
            v.take_requests().is_empty(),
            "rien à lister dans un dépôt vide"
        );
        v.open_branches();
        assert!(matches!(v.mode, Mode::Dir), "pas de sélecteur sans branche");
    }

    #[test]
    fn echec_au_chargement_des_branches() {
        let mut b = Browser::open(repo());
        b.take_requests();
        b.on_branches(&repo(), Err(Error::Network("coupé".into())));
        assert!(matches!(&b.dir, DirState::Failed(m) if m.contains("coupé")));
        // `r` redemande les branches tant que la branche par défaut est inconnue
        b.refresh();
        assert_eq!(b.take_requests(), [Request::Branches(repo())]);
        assert!(matches!(b.dir, DirState::Loading));
    }

    #[test]
    fn entrer_dans_un_dossier_puis_remonter_resélectionne_l_origine() {
        let mut b = browsing();
        b.move_selection(1); // « src »
        b.enter();
        assert_eq!(b.path, "src");
        assert!(matches!(b.dir, DirState::Loading));
        assert_eq!(
            b.take_requests(),
            [Request::Dir {
                repo: repo(),
                reference: "main".into(),
                path: "src".into()
            }]
        );
        b.on_dir(
            &repo(),
            "main",
            "src",
            Ok(vec![entry("main.rs", "src/main.rs", EntryKind::File, 5)]),
        );
        assert_eq!(b.selected, 0);
        b.up();
        assert_eq!(b.path, "");
        b.take_requests();
        b.on_dir(
            &repo(),
            "main",
            "",
            Ok(vec![
                entry("alpha", "alpha", EntryKind::Dir, 0),
                entry("src", "src", EntryKind::Dir, 0),
            ]),
        );
        assert_eq!(
            b.entries()[b.selected].name,
            "src",
            "on retombe sur le dossier quitté"
        );
    }

    #[test]
    fn le_decalage_de_la_liste_est_remis_a_zero_a_chaque_dossier() {
        let mut b = browsing();
        b.list_offset.set(100); // laissé par le rendu d'un long dossier
        b.picker_offset.set(40);
        b.move_selection(1);
        b.enter(); // « src »
        b.on_dir(
            &repo(),
            "main",
            "src",
            Ok(vec![entry("a.rs", "src/a.rs", EntryKind::File, 1)]),
        );
        assert_eq!(b.list_offset.get(), 0);
        b.open_branches();
        assert_eq!(b.picker_offset.get(), 0);
    }

    #[test]
    fn fil_d_ariane_de_l_explorateur() {
        let mut b = Browser::open(repo());
        assert_eq!(b.breadcrumb(), ["o/r", "Code", "…"], "branche inconnue");
        b.take_requests();
        b.on_branches(&repo(), Ok(vec![branch("feature/x", true)]));
        assert_eq!(
            b.breadcrumb(),
            ["o/r", "Code", "feature/x"],
            "un slash de branche reste un seul élément"
        );
        b.path = "src/deep".into();
        assert_eq!(b.breadcrumb(), ["o/r", "Code", "feature/x", "src", "deep"]);
        b.mode = Mode::File(FileView {
            path: "src/deep/a.rs".into(),
            text: TextView::new(),
        });
        assert_eq!(
            b.breadcrumb(),
            ["o/r", "Code", "feature/x", "src", "deep", "a.rs"]
        );
        b.path.clear();
        b.mode = Mode::Dir;
        assert_eq!(b.breadcrumb().len(), 3, "racine : pas de segment vide");
    }

    #[test]
    fn remonter_de_plusieurs_niveaux() {
        let mut b = browsing();
        b.path = "a/b/c".into();
        b.up();
        assert_eq!(b.path, "a/b");
        b.up();
        assert_eq!(b.path, "a");
        b.up();
        assert_eq!(b.path, "");
        b.take_requests();
        b.up(); // à la racine : sans effet
        assert!(b.take_requests().is_empty());
        assert_eq!(b.path, "");
    }

    #[test]
    fn ouvrir_un_fichier_et_revenir() {
        let mut b = browsing();
        b.move_selection(3); // « README.md »
        b.enter();
        let Mode::File(fv) = &b.mode else {
            panic!("pas un fichier")
        };
        assert_eq!(fv.path, "README.md");
        assert_eq!(
            b.take_requests(),
            [Request::File {
                repo: repo(),
                reference: "main".into(),
                path: "README.md".into()
            }]
        );
        b.on_file(
            &repo(),
            "main",
            "README.md",
            Ok(FileContent {
                size: 8,
                text: Some("# titre\n".into()),
                truncated: false,
            }),
        );
        let Mode::File(fv) = &b.mode else { panic!() };
        assert!(
            matches!(&fv.text.state, LogState::Ready(l) if l.len() == 1 && l[0].text == "# titre")
        );
        assert_eq!(fv.text.notice, None);
        assert!(!b.back(), "Échap ferme le fichier, pas l'explorateur");
        assert!(matches!(b.mode, Mode::Dir));
        assert_eq!(
            b.entries()[b.selected].name,
            "README.md",
            "la sélection n'a pas bougé"
        );
        assert!(b.back(), "Échap dans un dossier ferme l'explorateur");
    }

    #[test]
    fn liens_symboliques_et_sous_modules_ne_s_ouvrent_pas() {
        let mut b = browsing();
        b.move_selection(2); // « lien »
        b.enter();
        assert!(matches!(b.mode, Mode::Dir));
        assert!(b.info.as_deref().unwrap().contains("lien symbolique"));
        assert!(b.take_requests().is_empty());
        b.move_selection(2); // « sub »
        b.enter();
        assert!(b.info.as_deref().unwrap().contains("sous-module"));
        b.move_selection(-1);
        b.enter(); // un fichier : le message disparaît
        assert_eq!(b.info, None);
    }

    #[test]
    fn selecteur_de_branche() {
        let mut b = browsing();
        b.path = "src".into();
        b.open_branches();
        assert!(
            matches!(b.mode, Mode::Branches { selected: 0 }),
            "ouvert sur la branche courante"
        );
        assert!(
            b.take_requests().is_empty(),
            "les branches sont déjà chargées"
        );
        b.move_selection(1);
        b.enter();
        assert_eq!((b.reference.as_str(), b.path.as_str()), ("feature/x", ""));
        assert!(matches!(b.mode, Mode::Dir));
        assert_eq!(
            b.take_requests(),
            [Request::Dir {
                repo: repo(),
                reference: "feature/x".into(),
                path: String::new()
            }]
        );
        // Annuler le sélecteur
        b.open_branches();
        assert!(!b.back());
        assert!(matches!(b.mode, Mode::Dir));
        assert_eq!(b.reference, "feature/x");
    }

    #[test]
    fn selecteur_sans_branches_chargees() {
        let mut b = Browser::open(repo());
        b.take_requests();
        b.on_branches(&repo(), Ok(vec![branch("main", true)]));
        b.branches = None; // liste perdue (échec ultérieur d'un rafraîchissement)
        b.take_requests();
        b.open_branches();
        assert_eq!(b.take_requests(), [Request::Branches(repo())]);
        b.on_branches(&repo(), Err(Error::Network("coupé".into())));
        assert!(b.branches_error.as_deref().unwrap().contains("coupé"));
        assert!(
            matches!(b.dir, DirState::Loading | DirState::Ready(_)),
            "le dossier n'est pas perdu"
        );
    }

    #[test]
    fn reponses_perimees_ignorees() {
        let mut b = browsing();
        b.path = "src".into();
        let before = names(&b).len();
        // Mauvaise branche, mauvais dossier, mauvais dépôt
        b.on_dir(&repo(), "autre", "src", Ok(vec![]));
        b.on_dir(&repo(), "main", "docs", Ok(vec![]));
        b.on_dir(
            &Repo {
                owner: "x".into(),
                name: "y".into(),
            },
            "main",
            "src",
            Ok(vec![]),
        );
        assert_eq!(names(&b).len(), before);
        // Fichier : aucune vue ouverte, ou un autre fichier
        b.on_file(
            &repo(),
            "main",
            "a.txt",
            Ok(FileContent {
                size: 1,
                text: Some("x".into()),
                truncated: false,
            }),
        );
        assert!(matches!(b.mode, Mode::Dir));
        b.mode = Mode::File(FileView {
            path: "a.txt".into(),
            text: TextView::new(),
        });
        b.on_file(
            &repo(),
            "main",
            "b.txt",
            Ok(FileContent {
                size: 1,
                text: Some("x".into()),
                truncated: false,
            }),
        );
        let Mode::File(fv) = &b.mode else { panic!() };
        assert!(matches!(fv.text.state, LogState::Loading));
        b.on_branches(
            &Repo {
                owner: "x".into(),
                name: "y".into(),
            },
            Ok(vec![]),
        );
        assert!(!b.empty_repo);
    }

    #[test]
    fn echec_d_un_dossier_et_d_un_fichier() {
        let mut b = browsing();
        b.path = "src".into();
        b.on_dir(&repo(), "main", "src", Err(Error::NotFound("src".into())));
        assert!(matches!(&b.dir, DirState::Failed(m) if m.contains("src")));
        b.mode = Mode::File(FileView {
            path: "a.txt".into(),
            text: TextView::new(),
        });
        b.on_file(
            &repo(),
            "main",
            "a.txt",
            Err(Error::Network("coupé".into())),
        );
        let Mode::File(fv) = &b.mode else { panic!() };
        assert!(matches!(&fv.text.state, LogState::Failed(m) if m.contains("coupé")));
    }

    #[test]
    fn contenu_des_fichiers() {
        let show = |content: FileContent| {
            let mut v = TextView::new();
            show_file(&mut v, &content);
            let lines = match &v.state {
                LogState::Ready(l) => l.iter().map(|x| x.text.clone()).collect(),
                _ => vec![],
            };
            (lines, v.notice)
        };
        let text = |t: &str, truncated| FileContent {
            size: t.len() as u64,
            text: Some(t.into()),
            truncated,
        };
        // Texte ordinaire, avec et sans saut de ligne final
        assert_eq!(show(text("a\nb\n", false)).0, ["a", "b"]);
        assert_eq!(show(text("a\nb", false)).0, ["a", "b"]);
        assert_eq!(
            show(text("a\n\nb\n", false)).0,
            ["a", "", "b"],
            "les lignes vides internes restent"
        );
        // Fichier vide
        assert_eq!(show(text("", false)).0, ["(fichier vide)"]);
        // Fins de ligne Windows, retour chariot isolé, tabulations, échappements
        assert_eq!(show(text("un\r\ndeux\r\n", false)).0, ["un", "deux"]);
        assert_eq!(show(text("a\rb\n", false)).0, ["a\u{240d}b"]);
        assert_eq!(show(text("\tx\n", false)).0, ["    x"]);
        assert_eq!(show(text("\x1b[31mx\n", false)).0, ["\u{241b}[31mx"]);
        // Binaire
        let (lines, notice) = show(FileContent {
            size: 2051,
            text: None,
            truncated: false,
        });
        assert!(
            lines[0].contains("binaire") && lines[0].contains("2,0 Kio"),
            "{lines:?}"
        );
        assert_eq!(notice, None);
        // Tronqué en octets
        let (_, notice) = show(FileContent {
            size: 3 * 1024 * 1024,
            text: Some("abc\n".repeat(1000)),
            truncated: true,
        });
        let n = notice.unwrap();
        assert!(n.contains("tronqué") && n.contains("3,0 Mio"), "{n}");
    }

    #[test]
    fn plafond_de_lignes() {
        let mut v = TextView::new();
        let many = "\n".repeat(FILE_LIMIT_LINES + 10);
        show_file(
            &mut v,
            &FileContent {
                size: many.len() as u64,
                text: Some(many),
                truncated: false,
            },
        );
        let LogState::Ready(lines) = &v.state else {
            panic!()
        };
        assert_eq!(lines.len(), FILE_LIMIT_LINES);
        assert!(
            v.notice.as_deref().unwrap().contains("50000"),
            "{:?}",
            v.notice
        );
    }

    #[test]
    fn deplacement_borne() {
        let mut b = browsing();
        b.move_selection(-5);
        assert_eq!(b.selected, 0);
        b.select_last();
        assert_eq!(b.selected, 5);
        b.move_selection(3);
        assert_eq!(b.selected, 5);
        b.select_first();
        assert_eq!(b.selected, 0);
        // Dossier vide ou en chargement : pas de panique
        let mut empty = Browser::open(repo());
        empty.move_selection(3);
        empty.select_last();
        empty.enter();
        assert_eq!(empty.selected, 0);
    }

    #[test]
    fn actualiser() {
        let mut b = browsing();
        b.path = "src".into();
        b.refresh();
        assert_eq!(
            b.take_requests(),
            [Request::Dir {
                repo: repo(),
                reference: "main".into(),
                path: "src".into()
            }]
        );
        b.mode = Mode::File(FileView {
            path: "src/a.rs".into(),
            text: TextView::new(),
        });
        b.refresh();
        assert_eq!(
            b.take_requests(),
            [Request::File {
                repo: repo(),
                reference: "main".into(),
                path: "src/a.rs".into()
            }]
        );
    }

    #[test]
    fn tailles_lisibles() {
        assert_eq!(fmt_size(0), "0 o");
        assert_eq!(fmt_size(1023), "1023 o");
        assert_eq!(fmt_size(1024), "1,0 Kio");
        assert_eq!(fmt_size(1536), "1,5 Kio");
        assert_eq!(fmt_size(1_536_000), "1,5 Mio");
        assert_eq!(fmt_size(5 * 1024 * 1024 * 1024), "5,0 Gio");
        assert!(fmt_size(u64::MAX).ends_with("Gio"));
    }
}
