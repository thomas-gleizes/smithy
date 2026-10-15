//! Historique d'un dépôt : la liste des commits d'une branche (`CommitsView`), la liste des
//! branches (`BranchesView`) et un commit avec son correctif (`CommitView`). États purs comme les
//! autres pages : ils déposent des `Request` que la boucle principale exécute, et intègrent les
//! réponses en écartant celles qui ne les concernent plus (dépôt, branche ou page changés).

use std::cell::Cell;

use smithy_core::{
    Branch, ChangeKind, Commit, CommitDetail, CommitPage, Error, FileChange, Repo, Status,
    clean_text_line,
};

use crate::{
    highlight::{Run, Tok},
    logview::{LogLine, LogState, Part, TextView},
    request::Request,
    timeago,
};

/// On cesse de charger des pages au-delà : l'historique d'un gros dépôt se compte en dizaines de
/// milliers de commits, et chaque page est une requête.
pub const MAX_COMMITS: usize = 1000;
/// On demande la page suivante quand la sélection est à moins de cette distance de la fin.
const LOAD_AHEAD: usize = 5;
/// Plafonds du correctif affiché (octets lus, lignes montrées).
pub const PATCH_LIMIT_BYTES: usize = 1 << 20;
pub const PATCH_LIMIT_LINES: usize = 20_000;
/// Un fichier du diff d'au moins autant de lignes démarre replié.
const COLLAPSE_FROM_LINES: usize = 400;

/// Tous les mots du filtre (déjà en minuscules) sont-ils dans `haystack` ?
pub(crate) fn all_words(haystack: &str, words: &[String]) -> bool {
    let hay = haystack.to_lowercase();
    words.iter().all(|w| hay.contains(w.as_str()))
}

pub(crate) fn words_of(filter: &str) -> Vec<String> {
    filter.split_whitespace().map(str::to_lowercase).collect()
}

/// Saisie d'un filtre local, commune aux deux listes.
#[derive(Debug, Clone, Default)]
pub struct FilterInput {
    pub text: String,
    pub editing: bool,
    before: String,
}

impl FilterInput {
    pub fn start(&mut self) {
        self.editing = true;
        self.before = self.text.clone();
    }

    pub fn type_char(&mut self, c: char) -> bool {
        if self.editing {
            self.text.push(c);
        }
        self.editing
    }

    pub fn backspace(&mut self) -> bool {
        if self.editing {
            self.text.pop();
        }
        self.editing
    }

    pub fn commit(&mut self) {
        self.editing = false;
    }

    /// Échap pendant la saisie : le filtre validé d'avant revient.
    pub fn cancel(&mut self) {
        self.editing = false;
        self.text = std::mem::take(&mut self.before);
    }

    pub fn clear(&mut self) -> bool {
        let had = !self.text.is_empty();
        self.text.clear();
        self.editing = false;
        had
    }

    pub fn active(&self) -> bool {
        !self.text.trim().is_empty()
    }
}

// --- liste des commits

#[derive(Debug, Clone)]
pub struct CommitsView {
    pub repo: Repo,
    /// Branche demandée ; vide = la branche par défaut du dépôt.
    pub reference: String,
    pub default_branch: Option<String>,
    pub commits: Vec<Commit>,
    /// Prochaine page à demander (1 : rien n'est encore chargé).
    next_page: u32,
    pub has_more: bool,
    /// Une page est demandée et pas encore arrivée.
    pub loading: bool,
    pub error: Option<String>,
    /// Indice dans la liste **affichée** (`visible`).
    pub selected: usize,
    pub filter: FilterInput,
    pub branches: Option<Vec<Branch>>,
    /// Sélecteur de branche ouvert (indice de la ligne choisie).
    pub picker: Option<usize>,
    pub list_offset: Cell<usize>,
    pub picker_offset: Cell<usize>,
}

impl CommitsView {
    /// Ouvre l'historique d'une branche (`""` : la branche par défaut) et renvoie les requêtes.
    pub fn open(repo: Repo, reference: String) -> (Self, Vec<Request>) {
        let mut v = Self {
            repo,
            reference,
            default_branch: None,
            commits: Vec::new(),
            next_page: 1,
            has_more: false,
            loading: true,
            error: None,
            selected: 0,
            filter: FilterInput::default(),
            branches: None,
            picker: None,
            list_offset: Cell::new(0),
            picker_offset: Cell::new(0),
        };
        let requests = vec![v.page_request(1), Request::Branches(v.repo.clone())];
        v.next_page = 2;
        (v, requests)
    }

    fn page_request(&self, page: u32) -> Request {
        Request::Commits {
            repo: self.repo.clone(),
            reference: self.reference.clone(),
            page,
        }
    }

    /// La branche affichée : celle choisie, sinon la par défaut quand on la connaît.
    pub fn branch_label(&self) -> String {
        if !self.reference.is_empty() {
            self.reference.clone()
        } else {
            self.default_branch
                .clone()
                .unwrap_or_else(|| "branche par défaut".to_owned())
        }
    }

    pub fn visible(&self) -> Vec<&Commit> {
        let words = words_of(&self.filter.text);
        self.commits
            .iter()
            .filter(|c| all_words(&format!("{} {} {}", c.message, c.author, c.sha), &words))
            .collect()
    }

    pub fn selected_commit(&self) -> Option<&Commit> {
        self.visible().get(self.selected).copied()
    }

    /// Plus de pages à demander ? (il en reste, aucune n'est en route, on n'a pas atteint le plafond)
    fn wants_more(&self) -> bool {
        self.has_more && !self.loading && self.commits.len() < MAX_COMMITS && self.error.is_none()
    }

    /// Demande la page suivante si la sélection approche de la fin de la liste affichée.
    pub fn load_more_if_near_end(&mut self) -> Vec<Request> {
        let len = self.visible().len();
        if self.wants_more() && self.selected + LOAD_AHEAD >= len {
            self.loading = true;
            let page = self.next_page;
            return vec![self.page_request(page)];
        }
        Vec::new()
    }

    pub fn move_selection(&mut self, delta: isize) -> Vec<Request> {
        let max = self.visible().len().saturating_sub(1);
        self.selected = self.selected.min(max).saturating_add_signed(delta).min(max);
        self.load_more_if_near_end()
    }

    /// Une page arrive. La page 1 remplace la liste (la sélection suit le même commit) ; la
    /// suivante s'ajoute. Une réponse pour une autre branche ou une page inattendue est ignorée.
    pub fn apply_page(
        &mut self,
        repo: &Repo,
        reference: &str,
        page: u32,
        result: Result<CommitPage, Error>,
    ) -> Vec<Request> {
        if *repo != self.repo || reference != self.reference {
            return Vec::new();
        }
        if page > 1 && page != self.next_page {
            return Vec::new();
        }
        self.loading = false;
        match result {
            Ok(p) => {
                self.error = None;
                if page == 1 {
                    let keep = self.selected_commit().map(|c| c.sha.clone());
                    self.commits = p.commits;
                    self.next_page = 2;
                    self.selected = keep
                        .and_then(|sha| self.visible().iter().position(|c| c.sha == sha))
                        .unwrap_or(0);
                } else {
                    self.commits.extend(p.commits);
                    self.next_page += 1;
                }
                self.has_more = p.has_more;
                // Liste encore courte (filtre sévère, petite fenêtre) : on poursuit.
                self.load_more_if_near_end()
            }
            Err(e) => {
                self.error = Some(e.to_string());
                Vec::new()
            }
        }
    }

    pub fn apply_branches(&mut self, repo: &Repo, result: Result<Vec<Branch>, Error>) {
        if *repo != self.repo {
            return;
        }
        if let Ok(list) = result {
            self.default_branch = list.iter().find(|b| b.default).map(|b| b.name.clone());
            self.branches = Some(list);
        }
    }

    /// Touche `r` : recharge depuis la première page.
    pub fn refresh(&mut self) -> Vec<Request> {
        self.loading = true;
        self.error = None;
        vec![self.page_request(1), Request::Branches(self.repo.clone())]
    }

    // --- sélecteur de branche

    pub fn open_picker(&mut self) -> Vec<Request> {
        let current = self.branches.as_ref().and_then(|b| {
            let name = if self.reference.is_empty() {
                self.default_branch.as_deref()?
            } else {
                &self.reference
            };
            b.iter().position(|x| x.name == name)
        });
        self.picker = Some(current.unwrap_or(0));
        self.picker_offset.set(0);
        if self.branches.is_none() {
            vec![Request::Branches(self.repo.clone())]
        } else {
            Vec::new()
        }
    }

    pub fn picker_move(&mut self, delta: isize) {
        let max = self.branches.as_ref().map_or(0, Vec::len).saturating_sub(1);
        if let Some(sel) = self.picker.as_mut() {
            *sel = sel.saturating_add_signed(delta).min(max);
        }
    }

    /// Entrée dans le sélecteur : change de branche et recharge depuis la première page.
    pub fn picker_choose(&mut self) -> Vec<Request> {
        let Some(sel) = self.picker.take() else {
            return Vec::new();
        };
        let Some(name) = self
            .branches
            .as_ref()
            .and_then(|b| b.get(sel))
            .map(|b| b.name.clone())
        else {
            return Vec::new();
        };
        self.reference = name;
        self.commits.clear();
        self.has_more = false;
        self.next_page = 2;
        self.loading = true;
        self.error = None;
        self.selected = 0;
        self.filter.clear();
        self.list_offset.set(0);
        vec![self.page_request(1)]
    }

    // --- filtre

    pub fn filter_changed(&mut self) -> Vec<Request> {
        self.selected = 0;
        self.list_offset.set(0);
        self.load_more_if_near_end()
    }
}

// --- liste des branches

#[derive(Debug, Clone)]
pub struct BranchesView {
    pub repo: Repo,
    /// `None` : pas encore reçues.
    pub branches: Option<Vec<Branch>>,
    pub error: Option<String>,
    pub selected: usize,
    pub filter: FilterInput,
    pub list_offset: Cell<usize>,
}

impl BranchesView {
    pub fn open(repo: Repo) -> (Self, Vec<Request>) {
        let requests = vec![Request::Branches(repo.clone())];
        (
            Self {
                repo,
                branches: None,
                error: None,
                selected: 0,
                filter: FilterInput::default(),
                list_offset: Cell::new(0),
            },
            requests,
        )
    }

    pub fn visible(&self) -> Vec<&Branch> {
        let words = words_of(&self.filter.text);
        self.branches
            .iter()
            .flatten()
            .filter(|b| {
                let head = b.head.as_ref();
                all_words(
                    &format!(
                        "{} {} {}",
                        b.name,
                        head.map_or("", |h| h.message.as_str()),
                        head.map_or("", |h| h.author.as_str())
                    ),
                    &words,
                )
            })
            .collect()
    }

    pub fn selected_branch(&self) -> Option<&Branch> {
        self.visible().get(self.selected).copied()
    }

    pub fn move_selection(&mut self, delta: isize) {
        let max = self.visible().len().saturating_sub(1);
        self.selected = self.selected.min(max).saturating_add_signed(delta).min(max);
    }

    pub fn apply_branches(&mut self, repo: &Repo, result: Result<Vec<Branch>, Error>) {
        if *repo != self.repo {
            return;
        }
        match result {
            Ok(list) => {
                self.error = None;
                let keep = self.selected_branch().map(|b| b.name.clone());
                self.branches = Some(list);
                self.selected = keep
                    .and_then(|n| self.visible().iter().position(|b| b.name == n))
                    .unwrap_or(0);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn refresh(&mut self) -> Vec<Request> {
        self.error = None;
        vec![Request::Branches(self.repo.clone())]
    }

    pub fn filter_changed(&mut self) {
        self.selected = 0;
        self.list_offset.set(0);
    }
}

// --- un commit et son correctif

#[derive(Debug, Clone)]
pub struct CommitView {
    pub repo: Repo,
    pub sha: String,
    /// Titre court pour l'en-tête, connu dès l'ouverture (vient de la liste).
    pub summary: String,
    pub text: TextView,
}

impl CommitView {
    pub fn open(repo: Repo, commit: &Commit, wrap: bool) -> (Self, Vec<Request>) {
        let mut text = TextView::new();
        text.wrap = wrap;
        let request = Request::CommitDetail {
            repo: repo.clone(),
            sha: commit.sha.clone(),
        };
        (
            Self {
                repo,
                sha: commit.sha.clone(),
                summary: commit.summary().to_owned(),
                text,
            },
            vec![request],
        )
    }

    pub fn refresh(&self) -> Vec<Request> {
        vec![Request::CommitDetail {
            repo: self.repo.clone(),
            sha: self.sha.clone(),
        }]
    }

    pub fn apply_detail(&mut self, repo: &Repo, sha: &str, result: Result<CommitDetail, Error>) {
        if *repo != self.repo || sha != self.sha {
            return;
        }
        match result {
            Ok(d) => {
                let (preface, parts, notice) = diff_layout(&d, timeago::now_epoch());
                self.text.set_sectioned(preface, parts);
                self.text.notice = notice;
            }
            Err(e) => self.text.state = LogState::Failed(e.to_string()),
        }
    }
}

pub(crate) fn plain(text: impl Into<String>, tok: Option<Tok>) -> LogLine {
    let text = clean_text_line(&text.into());
    let mut l = LogLine::text(text);
    if let Some(tok) = tok {
        l.runs = vec![Run {
            start: 0,
            end: l.text.len() as u32,
            tok,
        }];
    }
    l
}

/// Un message, une ligne de texte par ligne ; replié par mots.
fn message_lines(message: &str) -> Vec<LogLine> {
    message
        .trim_end()
        .lines()
        .map(|l| {
            let mut line = plain(l, None);
            line.words = true;
            line
        })
        .collect()
}

fn kind_status(kind: ChangeKind) -> Status {
    match kind {
        ChangeKind::Added => Status::Success,
        ChangeKind::Removed => Status::Failure,
        ChangeKind::Modified => Status::Running,
        ChangeKind::Renamed => Status::Pending,
        ChangeKind::Other => Status::Unknown,
    }
}

fn diff_line(raw: &str) -> LogLine {
    let tok = if raw.starts_with("+++") || raw.starts_with("---") {
        Some(Tok::Meta)
    } else if raw.starts_with('+') {
        Some(Tok::Added)
    } else if raw.starts_with('-') {
        Some(Tok::Removed)
    } else if raw.starts_with("@@") {
        Some(Tok::Hunk)
    } else if raw.starts_with('\\')
        || raw.starts_with("new file")
        || raw.starts_with("deleted file")
        || raw.starts_with("rename ")
        || raw.starts_with("similarity")
        || raw.starts_with("copy ")
        || raw.starts_with("old mode")
        || raw.starts_with("new mode")
        || raw.starts_with("Binary files")
    {
        Some(Tok::Meta)
    } else {
        None
    };
    plain(raw, tok)
}

/// Nom du fichier d'un en-tête `diff --git a/X b/Y` : Y, sauf pour un fichier supprimé.
fn file_of_header(header: &str) -> String {
    let rest = header.trim_start_matches("diff --git ");
    match rest.split_once(" b/") {
        Some((a, b)) => {
            let a = a.trim_start_matches("a/");
            if b.is_empty() {
                a.to_owned()
            } else {
                b.to_owned()
            }
        }
        None => rest.to_owned(),
    }
}

/// Met le commit en forme : un en-tête libre (empreinte, auteur, date, parents, message,
/// bilan), puis une section pliable par fichier du correctif. Renvoie aussi l'avertissement à
/// montrer quand le correctif a été coupé.
pub fn diff_layout(d: &CommitDetail, now: i64) -> (Vec<LogLine>, Vec<Part>, Option<String>) {
    let c = &d.commit;
    let mut head = vec![plain(format!("commit {}", c.sha), Some(Tok::Heading))];
    head.push(plain(format!("Auteur : {}", c.author), None));
    if let Some(date) = &c.date {
        head.push(plain(
            format!("Date   : {date}  ({})", timeago::ago_rfc3339(now, date)),
            None,
        ));
    }
    let parents: Vec<String> = d
        .parents
        .iter()
        .map(|p| p[..p.len().min(7)].to_owned())
        .collect();
    head.push(plain(
        if parents.is_empty() {
            "Parent : aucun (commit racine)".to_owned()
        } else {
            format!("Parent : {}", parents.join(", "))
        },
        None,
    ));
    head.push(plain("", None));
    head.extend(message_lines(&c.message));
    head.push(plain("", None));
    let n = d.files.len();
    head.push(plain(
        format!(
            "{n} fichier{} modifié{} · +{} −{}",
            if n > 1 { "s" } else { "" },
            if n > 1 { "s" } else { "" },
            d.additions,
            d.deletions
        ),
        Some(Tok::Strong),
    ));
    head.push(plain("", None));

    let (parts, line_cut) = diff_parts(&d.patch, &d.files);
    if parts.is_empty() {
        head.push(plain(
            "(aucun changement de fichier à afficher)",
            Some(Tok::Meta),
        ));
    }
    let notice = if d.patch_truncated || line_cut {
        Some("correctif tronqué : début affiché".to_owned())
    } else {
        None
    };
    (head, parts, notice)
}

/// Découpe un correctif en une section pliable par fichier (titre : chemin et bilan +/−,
/// couleur : nature du changement, d'après `files` ou, à défaut, les en-têtes du diff).
/// Renvoie aussi si le plafond de lignes a coupé le correctif.
pub fn diff_parts(patch: &str, files: &[FileChange]) -> (Vec<Part>, bool) {
    struct Block {
        path: String,
        lines: Vec<LogLine>,
        adds: usize,
        dels: usize,
        kind_hint: Option<ChangeKind>,
    }
    let mut blocks: Vec<Block> = Vec::new();
    let mut total = 0usize;
    let mut line_cut = false;
    for raw in patch.lines() {
        if raw.starts_with("diff --git ") {
            blocks.push(Block {
                path: file_of_header(raw),
                lines: Vec::new(),
                adds: 0,
                dels: 0,
                kind_hint: None,
            });
            continue;
        }
        let Some(b) = blocks.last_mut() else { continue };
        // L'empreinte des blobs n'apporte rien à la lecture.
        if raw.starts_with("index ") {
            continue;
        }
        if total >= PATCH_LIMIT_LINES {
            line_cut = true;
            break;
        }
        total += 1;
        if raw.starts_with("new file mode") {
            b.kind_hint = Some(ChangeKind::Added);
        } else if raw.starts_with("deleted file mode") {
            b.kind_hint = Some(ChangeKind::Removed);
        } else if raw.starts_with("rename from") {
            b.kind_hint = Some(ChangeKind::Renamed);
        } else if raw.starts_with('+') && !raw.starts_with("+++") {
            b.adds += 1;
        } else if raw.starts_with('-') && !raw.starts_with("---") {
            b.dels += 1;
        }
        b.lines.push(diff_line(raw));
    }

    let parts: Vec<Part> = blocks
        .into_iter()
        .map(|b| {
            let kind = files
                .iter()
                .find(|f| f.path == b.path)
                .map(|f| f.kind)
                .or(b.kind_hint)
                .unwrap_or(ChangeKind::Modified);
            Part {
                name: format!("{}  +{} −{}", b.path, b.adds, b.dels),
                status: kind_status(kind),
                collapsed: b.lines.len() >= COLLAPSE_FROM_LINES,
                lines: b.lines,
            }
        })
        .collect();
    (parts, line_cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_core::BranchHead;

    fn repo() -> Repo {
        Repo {
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn commit(n: usize) -> Commit {
        Commit {
            sha: format!("{n:040x}"),
            message: format!("feat: n°{n}\n\ncorps {n}\n"),
            author: if n.is_multiple_of(2) { "ana" } else { "bob" }.into(),
            date: Some("2026-10-14T10:00:00Z".into()),
            url: None,
            parents: vec![],
        }
    }

    fn page(range: std::ops::Range<usize>, more: bool) -> CommitPage {
        CommitPage {
            commits: range.map(commit).collect(),
            has_more: more,
        }
    }

    fn open() -> CommitsView {
        let (v, reqs) = CommitsView::open(repo(), String::new());
        assert_eq!(
            reqs,
            [
                Request::Commits {
                    repo: repo(),
                    reference: String::new(),
                    page: 1
                },
                Request::Branches(repo())
            ]
        );
        v
    }

    #[test]
    fn pagination_progressive_vers_la_fin_de_la_liste() {
        let mut v = open();
        assert!(v.loading);
        // Page 1 : sélection au début, rien à demander
        let more = v.apply_page(&repo(), "", 1, Ok(page(0..50, true)));
        assert!(more.is_empty() && !v.loading && v.commits.len() == 50);
        // On descend : à 5 de la fin, la page 2 est demandée, une seule fois
        let mut asked = Vec::new();
        for _ in 0..49 {
            asked.extend(v.move_selection(1));
        }
        assert_eq!(asked.len(), 1, "une seule demande malgré plusieurs pas");
        assert_eq!(
            asked[0],
            Request::Commits {
                repo: repo(),
                reference: String::new(),
                page: 2
            }
        );
        assert!(v.loading && v.move_selection(0).is_empty());
        // La page 2 s'ajoute ; une page déjà reçue (ou hors ordre) est ignorée
        v.apply_page(&repo(), "", 2, Ok(page(50..63, false)));
        assert_eq!(v.commits.len(), 63);
        assert!(!v.has_more);
        assert!(
            v.apply_page(&repo(), "", 2, Ok(page(0..5, true)))
                .is_empty()
        );
        assert_eq!(v.commits.len(), 63);
        assert!(v.move_selection(100).is_empty(), "plus rien à charger");
    }

    #[test]
    fn plafond_de_commits_charges() {
        let mut v = open();
        v.apply_page(&repo(), "", 1, Ok(page(0..MAX_COMMITS, true)));
        assert!(v.move_selection(isize::MAX / 2).is_empty());
    }

    #[test]
    fn reponses_d_une_autre_branche_ou_d_un_autre_depot_ignorees() {
        let mut v = open();
        v.apply_page(&repo(), "autre", 1, Ok(page(0..3, false)));
        assert!(v.commits.is_empty() && v.loading);
        let other = Repo {
            owner: "x".into(),
            name: "y".into(),
        };
        v.apply_page(&other, "", 1, Ok(page(0..3, false)));
        assert!(v.commits.is_empty());
        v.apply_page(&repo(), "", 1, Err(Error::Network("coupé".into())));
        assert!(v.error.as_deref().unwrap().contains("coupé") && !v.loading);
    }

    #[test]
    fn filtre_local_et_chargement_progressif_quand_la_liste_est_courte() {
        let mut v = open();
        v.apply_page(&repo(), "", 1, Ok(page(0..50, true)));
        v.filter.start();
        "ana n°48".chars().for_each(|c| {
            v.filter.type_char(c);
        });
        let reqs = v.filter_changed();
        // « ana » (pairs) et « n°48 » : un seul résultat → liste courte, on demande la suite
        assert_eq!(v.visible().len(), 1);
        assert_eq!(
            reqs.len(),
            1,
            "liste courte : la page suivante est demandée"
        );
        let ids: Vec<_> = v.visible().iter().map(|c| c.summary().to_owned()).collect();
        assert!(ids.contains(&"feat: n°48".to_owned()));
        assert!(!ids.contains(&"feat: n°5".to_owned()));
        // Sans casse, sur l'auteur, le message ou l'empreinte
        v.filter.clear();
        v.filter.start();
        "BOB".chars().for_each(|c| {
            v.filter.type_char(c);
        });
        assert!(v.visible().iter().all(|c| c.author == "bob"));
        v.filter.cancel();
        assert_eq!(v.filter.text, "");
    }

    #[test]
    fn la_selection_suit_le_meme_commit_au_rechargement() {
        let mut v = open();
        v.apply_page(&repo(), "", 1, Ok(page(0..10, false)));
        v.move_selection(3);
        let sha = v.selected_commit().unwrap().sha.clone();
        v.refresh();
        // De nouveaux commits arrivent en tête
        let mut p = page(100..103, false);
        p.commits.extend(page(0..10, false).commits);
        v.apply_page(&repo(), "", 1, Ok(p));
        assert_eq!(v.selected_commit().unwrap().sha, sha);
    }

    #[test]
    fn selecteur_de_branche() {
        let mut v = open();
        v.apply_page(&repo(), "", 1, Ok(page(0..5, false)));
        v.apply_branches(
            &repo(),
            Ok(vec![
                Branch::new("main", true),
                Branch::new("dev", false),
                Branch::new("feature/x", false),
            ]),
        );
        assert_eq!(v.branch_label(), "main", "la branche par défaut est nommée");
        assert!(v.open_picker().is_empty());
        assert_eq!(v.picker, Some(0), "commence sur la branche courante");
        v.picker_move(2);
        let reqs = v.picker_choose();
        assert_eq!(v.reference, "feature/x");
        assert_eq!(
            reqs,
            [Request::Commits {
                repo: repo(),
                reference: "feature/x".into(),
                page: 1
            }]
        );
        assert!(v.commits.is_empty() && v.loading && v.picker.is_none());
        // Une page de l'ancienne branche qui arrive tard est ignorée
        v.apply_page(&repo(), "", 1, Ok(page(0..5, false)));
        assert!(v.commits.is_empty());
        v.apply_page(&repo(), "feature/x", 1, Ok(page(0..2, false)));
        assert_eq!(v.commits.len(), 2);
        // Sans liste de branches, le sélecteur la demande
        let (mut w, _) = CommitsView::open(repo(), String::new());
        assert_eq!(w.open_picker(), [Request::Branches(repo())]);
    }

    fn branch(name: &str, default: bool, msg: &str) -> Branch {
        Branch {
            head: Some(BranchHead {
                sha: "abcdef0123".into(),
                message: msg.into(),
                author: "ana".into(),
                date: None,
            }),
            ..Branch::new(name, default)
        }
    }

    #[test]
    fn liste_des_branches_filtre_et_selection() {
        let (mut v, reqs) = BranchesView::open(repo());
        assert_eq!(reqs, [Request::Branches(repo())]);
        assert!(v.visible().is_empty() && v.branches.is_none());
        v.apply_branches(
            &repo(),
            Ok(vec![
                branch("main", true, "fix: bug"),
                branch("dev", false, "feat: nouveauté"),
                branch("hotfix/1", false, "fix: urgent"),
            ]),
        );
        assert_eq!(v.visible().len(), 3);
        v.filter.start();
        "fix".chars().for_each(|c| {
            v.filter.type_char(c);
        });
        v.filter_changed();
        let names: Vec<_> = v.visible().iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            ["main", "hotfix/1"],
            "le message du dernier commit compte"
        );
        v.move_selection(10);
        assert_eq!(v.selected_branch().unwrap().name, "hotfix/1");
        // Un rechargement garde la même branche sélectionnée
        v.apply_branches(
            &repo(),
            Ok(vec![
                branch("a-new", false, "fix: x"),
                branch("main", true, "fix: bug"),
                branch("hotfix/1", false, "fix: urgent"),
            ]),
        );
        assert_eq!(v.selected_branch().unwrap().name, "hotfix/1");
        v.apply_branches(&repo(), Err(Error::Network("hs".into())));
        assert!(
            v.error.is_some() && v.branches.is_some(),
            "l'ancienne liste reste"
        );
    }

    // --- diff

    fn detail(patch: &str) -> CommitDetail {
        CommitDetail {
            commit: Commit {
                message: "fix: titre\n\nCorps\nsur deux lignes\n".into(),
                ..commit(7)
            },
            parents: vec!["aaaa1111bbbb".into()],
            files: vec![
                FileChange {
                    path: "a.txt".into(),
                    kind: ChangeKind::Modified,
                },
                FileChange {
                    path: "n.txt".into(),
                    kind: ChangeKind::Added,
                },
            ],
            additions: 3,
            deletions: 1,
            patch: patch.into(),
            patch_truncated: false,
        }
    }

    const PATCH: &str = "diff --git a/a.txt b/a.txt\nindex 111..222 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n contexte\n-vieux\n+neuf\n+autre\ndiff --git a/n.txt b/n.txt\nnew file mode 100644\nindex 0000000..333\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+tout neuf\n";

    #[test]
    fn mise_en_forme_du_commit_par_fichier() {
        let (head, parts, notice) = diff_layout(&detail(PATCH), timeago::now_epoch());
        let texts: Vec<_> = head.iter().map(|l| l.text.as_str()).collect();
        assert!(texts[0].starts_with("commit 0000"));
        assert!(texts.contains(&"Parent : aaaa111"));
        assert!(texts.contains(&"fix: titre") && texts.contains(&"sur deux lignes"));
        assert!(texts.contains(&"2 fichiers modifiés · +3 −1"));
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("Date   : 2026-10-14T10:00:00Z"))
        );
        assert_eq!(notice, None);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "a.txt  +2 −1");
        assert_eq!(parts[1].name, "n.txt  +1 −0");
        // Les lignes `diff --git` et `index` disparaissent ; le reste est gardé
        let first: Vec<_> = parts[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            first,
            [
                "--- a/a.txt",
                "+++ b/a.txt",
                "@@ -1,2 +1,3 @@",
                " contexte",
                "-vieux",
                "+neuf",
                "+autre"
            ]
        );
        let toks: Vec<_> = parts[0]
            .lines
            .iter()
            .map(|l| l.runs.first().map(|r| r.tok))
            .collect();
        assert_eq!(
            toks,
            [
                Some(Tok::Meta),
                Some(Tok::Meta),
                Some(Tok::Hunk),
                None,
                Some(Tok::Removed),
                Some(Tok::Added),
                Some(Tok::Added)
            ]
        );
        // Statut : modifié = jaune, ajouté = vert (d'après la liste de fichiers du JSON)
        assert_eq!(parts[0].status, Status::Running);
        assert_eq!(parts[1].status, Status::Success);
        assert!(parts.iter().all(|p| !p.collapsed));
        // Les régions couvrent toute la ligne, sur des frontières de caractères
        for p in &parts {
            for l in &p.lines {
                for r in &l.runs {
                    assert!(l.text.is_char_boundary(r.end as usize));
                    assert_eq!(r.start, 0);
                }
            }
        }
    }

    #[test]
    fn fichier_supprime_binaire_renomme_et_commit_vide() {
        let patch = "diff --git a/old.txt b/old.txt\ndeleted file mode 100644\nindex 1..0\n--- a/old.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\ndiff --git a/d.bin b/d.bin\nindex 1..2 100644\nBinary files a/d.bin and b/d.bin differ\ndiff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";
        let mut d = detail(patch);
        d.files.clear();
        let (_, parts, _) = diff_layout(&d, 0);
        let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["old.txt  +0 −1", "d.bin  +0 −0", "y  +0 −0"]);
        // Sans liste de fichiers, la nature se déduit du correctif
        assert_eq!(parts[0].status, Status::Failure, "supprimé");
        assert_eq!(parts[1].status, Status::Running, "binaire modifié");
        assert_eq!(parts[2].status, Status::Pending, "renommé");
        assert!(
            parts[1]
                .lines
                .iter()
                .any(|l| l.text.starts_with("Binary files"))
        );
        // Commit sans correctif (vide, ou fusion)
        let (head, parts, _) = diff_layout(&detail(""), 0);
        assert!(parts.is_empty());
        assert!(head.last().unwrap().text.contains("aucun changement"));
        // Sans parent : commit racine
        let mut root = detail(PATCH);
        root.parents.clear();
        let (head, _, _) = diff_layout(&root, 0);
        assert!(head.iter().any(|l| l.text.contains("commit racine")));
    }

    #[test]
    fn grand_correctif_borne_et_gros_fichier_replie() {
        let mut patch =
            String::from("diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -0,0 +1,100000 @@\n");
        for i in 0..(PATCH_LIMIT_LINES + 500) {
            patch.push_str(&format!("+ligne {i}\n"));
        }
        let (_, parts, notice) = diff_layout(&detail(&patch), 0);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].lines.len() <= PATCH_LIMIT_LINES);
        assert!(parts[0].collapsed, "un gros fichier démarre replié");
        assert!(notice.unwrap().contains("tronqué"));
        // Coupé par le serveur (plafond d'octets)
        let mut d = detail(PATCH);
        d.patch_truncated = true;
        assert!(diff_layout(&d, 0).2.is_some());
    }

    #[test]
    fn caracteres_de_controle_et_tabulations_du_correctif() {
        let patch = "diff --git a/t b/t\n--- a/t\n+++ b/t\n@@ -1 +1 @@\n-\tvieux\x1b\n+\tneuf\r\n";
        let (_, parts, _) = diff_layout(&detail(patch), 0);
        let lines: Vec<_> = parts[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert!(lines[3].starts_with("-    vieux\u{241b}"), "{lines:?}");
        assert_eq!(lines[4], "+    neuf");
        // La couleur couvre le texte nettoyé
        let r = parts[0].lines[3].runs[0];
        assert_eq!(r.end as usize, parts[0].lines[3].text.len());
    }

    #[test]
    fn la_vue_d_un_commit_ignore_les_reponses_d_un_autre_commit() {
        let (mut v, reqs) = CommitView::open(repo(), &commit(7), true);
        assert_eq!(
            reqs,
            [Request::CommitDetail {
                repo: repo(),
                sha: commit(7).sha
            }]
        );
        assert!(v.text.wrap && matches!(v.text.state, LogState::Loading));
        v.apply_detail(&repo(), "autre", Ok(detail(PATCH)));
        assert!(matches!(v.text.state, LogState::Loading));
        v.apply_detail(&repo(), &commit(7).sha, Ok(detail(PATCH)));
        assert!(matches!(v.text.state, LogState::Ready(_)));
        v.apply_detail(&repo(), &commit(7).sha, Err(Error::Network("hs".into())));
        assert!(matches!(&v.text.state, LogState::Failed(m) if m.contains("hs")));
    }
}
