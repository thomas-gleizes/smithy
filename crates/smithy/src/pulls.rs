//! Pull requests d'un dépôt : la liste (`PullsView`, filtrée par état côté serveur et par texte
//! localement) et une PR (`PullView` : un volet de texte avec une section pliable pour les
//! revues, la conversation, les commits, la CI et chaque fichier du correctif). États purs
//! comme l'historique : ils déposent des `Request` et ignorent les réponses qui ne les
//! concernent plus (dépôt, état, page ou numéro changés).

use std::cell::Cell;

use smithy_core::{
    Error, PullDetail, PullFilter, PullPage, PullRequest, PullState, Repo, Review, ReviewState,
    Status,
};

use crate::{
    highlight::Tok,
    history::{FilterInput, all_words, diff_parts, plain, words_of},
    logview::{LogLine, LogState, Part, TextView},
    markdown,
    request::Request,
    timeago,
};

/// Plafonds du correctif d'une PR (octets lus) : comme celui d'un commit.
pub const PATCH_LIMIT_BYTES: usize = crate::history::PATCH_LIMIT_BYTES;
/// On cesse de charger des pages au-delà.
const MAX_PULLS: usize = 1000;
/// On demande la page suivante quand la sélection est à moins de cette distance de la fin.
const LOAD_AHEAD: usize = 5;
/// Au-delà, la section des commits démarre repliée.
const COLLAPSE_COMMITS_FROM: usize = 10;

pub fn filter_label(filter: PullFilter) -> &'static str {
    match filter {
        PullFilter::Open => "ouvertes",
        PullFilter::Closed => "fermées",
        PullFilter::All => "toutes",
    }
}

pub fn state_label(p: &PullRequest) -> &'static str {
    match (p.state, p.draft) {
        (PullState::Open, true) => "brouillon",
        (PullState::Open, false) => "ouverte",
        (PullState::Merged, _) => "fusionnée",
        (PullState::Closed, _) => "fermée",
    }
}

/// Couleur d'une PR dans la liste et dans son en-tête, avec les teintes des statuts.
pub fn state_status(p: &PullRequest) -> Status {
    match (p.state, p.draft) {
        (PullState::Open, true) => Status::Pending,
        (PullState::Open, false) => Status::Success,
        (PullState::Merged, _) => Status::Running,
        (PullState::Closed, _) => Status::Failure,
    }
}

// --- liste

#[derive(Debug, Clone)]
pub struct PullsView {
    pub repo: Repo,
    /// État demandé au serveur (`s` le change).
    pub state: PullFilter,
    pub pulls: Vec<PullRequest>,
    /// Prochaine page à demander.
    next_page: u32,
    pub has_more: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// Indice dans la liste **affichée** (`visible`).
    pub selected: usize,
    pub filter: FilterInput,
    pub list_offset: Cell<usize>,
}

impl PullsView {
    pub fn open(repo: Repo, state: PullFilter) -> (Self, Vec<Request>) {
        let mut v = Self {
            repo,
            state,
            pulls: Vec::new(),
            next_page: 2,
            has_more: false,
            loading: true,
            error: None,
            selected: 0,
            filter: FilterInput::default(),
            list_offset: Cell::new(0),
        };
        let request = v.page_request(1);
        v.loading = true;
        (v, vec![request])
    }

    fn page_request(&self, page: u32) -> Request {
        Request::Pulls {
            repo: self.repo.clone(),
            filter: self.state,
            page,
        }
    }

    /// Filtre local : numéro (`#12`), titre, auteur, branches, étiquettes, état.
    pub fn visible(&self) -> Vec<&PullRequest> {
        let words = words_of(&self.filter.text);
        self.pulls
            .iter()
            .filter(|p| {
                let hay = format!(
                    "#{} {} {} {} {} {} {}",
                    p.number,
                    p.title,
                    p.author,
                    p.head,
                    p.base,
                    p.labels.join(" "),
                    state_label(p)
                );
                all_words(&hay, &words)
            })
            .collect()
    }

    pub fn selected_pull(&self) -> Option<&PullRequest> {
        self.visible().get(self.selected).copied()
    }

    fn wants_more(&self) -> bool {
        self.has_more && !self.loading && self.pulls.len() < MAX_PULLS && self.error.is_none()
    }

    pub fn load_more_if_near_end(&mut self) -> Vec<Request> {
        let len = self.visible().len();
        if self.wants_more() && self.selected + LOAD_AHEAD >= len {
            self.loading = true;
            return vec![self.page_request(self.next_page)];
        }
        Vec::new()
    }

    pub fn move_selection(&mut self, delta: isize) -> Vec<Request> {
        let max = self.visible().len().saturating_sub(1);
        self.selected = self.selected.min(max).saturating_add_signed(delta).min(max);
        self.load_more_if_near_end()
    }

    /// Une page arrive. La page 1 remplace la liste (la sélection suit la même PR) ; la
    /// suivante s'ajoute. Une réponse pour un autre état ou une page inattendue est ignorée.
    pub fn apply_page(
        &mut self,
        repo: &Repo,
        state: PullFilter,
        page: u32,
        result: Result<PullPage, Error>,
    ) -> Vec<Request> {
        if *repo != self.repo || state != self.state {
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
                    let keep = self.selected_pull().map(|p| p.number);
                    self.pulls = p.pulls;
                    self.next_page = 2;
                    self.selected = keep
                        .and_then(|n| self.visible().iter().position(|p| p.number == n))
                        .unwrap_or(0);
                } else {
                    self.pulls.extend(p.pulls);
                    self.next_page += 1;
                }
                self.has_more = p.has_more;
                self.load_more_if_near_end()
            }
            Err(e) => {
                self.error = Some(e.to_string());
                Vec::new()
            }
        }
    }

    /// Touche `r` : recharge depuis la première page.
    pub fn refresh(&mut self) -> Vec<Request> {
        self.loading = true;
        self.error = None;
        vec![self.page_request(1)]
    }

    /// Touche `s` : ouvertes → fermées → toutes, rechargé depuis la première page.
    pub fn cycle_state(&mut self) -> Vec<Request> {
        self.state = match self.state {
            PullFilter::Open => PullFilter::Closed,
            PullFilter::Closed => PullFilter::All,
            PullFilter::All => PullFilter::Open,
        };
        self.pulls.clear();
        self.has_more = false;
        self.next_page = 2;
        self.selected = 0;
        self.list_offset.set(0);
        self.refresh()
    }

    pub fn filter_changed(&mut self) -> Vec<Request> {
        self.selected = 0;
        self.list_offset.set(0);
        self.load_more_if_near_end()
    }
}

// --- une PR

#[derive(Debug, Clone)]
pub struct PullView {
    pub repo: Repo,
    pub number: u64,
    /// Titre pour l'en-tête, connu dès l'ouverture quand on vient de la liste.
    pub title: String,
    /// Branche proposée, une fois connue (`e` ouvre son code).
    pub head: Option<String>,
    pub text: TextView,
}

impl PullView {
    pub fn open(repo: Repo, number: u64, title: String) -> (Self, Vec<Request>) {
        let mut text = TextView::new();
        // Description et commentaires sont de la prose : sans retour à la ligne, ils seraient
        // coupés au bord. `w` le retire pour lire un diff large.
        text.wrap = true;
        let v = Self {
            repo,
            number,
            title,
            head: None,
            text,
        };
        let requests = v.refresh();
        (v, requests)
    }

    pub fn refresh(&self) -> Vec<Request> {
        vec![Request::PullDetail {
            repo: self.repo.clone(),
            number: self.number,
        }]
    }

    pub fn apply_detail(&mut self, repo: &Repo, number: u64, result: Result<PullDetail, Error>) {
        if *repo != self.repo || number != self.number {
            return;
        }
        match result {
            Ok(d) => {
                self.title = d.pull.title.clone();
                self.head = Some(d.pull.head.clone()).filter(|h| !h.is_empty());
                let (preface, parts, notice) = pull_layout(&d, timeago::now_epoch());
                self.text.set_sectioned(preface, parts);
                self.text.notice = notice;
            }
            Err(e) => self.text.state = LogState::Failed(e.to_string()),
        }
    }
}

fn ago(now: i64, date: Option<&str>) -> Option<String> {
    date.map(|d| timeago::ago_rfc3339(now, d))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n > 1 { many } else { one })
}

/// Texte en Markdown (description, revue, commentaire), mis en forme comme un fichier `.md`.
fn prose(src: &str) -> Vec<LogLine> {
    markdown::render(src, false)
        .into_iter()
        .map(markdown::MdLine::into_log_line)
        .collect()
}

fn review_verb(r: &Review) -> (&'static str, &'static str) {
    match r.state {
        ReviewState::Approved => ("✔", "a approuvé"),
        ReviewState::ChangesRequested => ("✘", "demande des changements"),
        ReviewState::Commented => ("●", "a commenté"),
        ReviewState::Requested => ("…", "revue demandée"),
        ReviewState::Other => ("●", "revue"),
    }
}

/// Bilan des revues en vigueur (ni écartées ni périmées) : couleur de la section.
fn reviews_status(reviews: &[Review]) -> Status {
    let live = reviews.iter().filter(|r| !r.dismissed && !r.stale);
    let states: Vec<ReviewState> = live.map(|r| r.state).collect();
    if states.contains(&ReviewState::ChangesRequested) {
        Status::Failure
    } else if states.contains(&ReviewState::Approved) {
        Status::Success
    } else if states.contains(&ReviewState::Requested) {
        Status::Pending
    } else {
        Status::Unknown
    }
}

fn check_symbol(s: Status) -> &'static str {
    match s {
        Status::Success => "✔",
        Status::Failure => "✘",
        Status::Running | Status::Pending => "●",
        _ => "?",
    }
}

fn check_word(s: Status) -> &'static str {
    match s {
        Status::Success => "succès",
        Status::Failure => "échec",
        Status::Running | Status::Pending => "en cours",
        _ => "inconnu",
    }
}

/// Met la PR en forme : un en-tête (état, branches, auteur, étiquettes, fusion, CI, bilan,
/// description), puis les sections pliables (revues, conversation, commits, CI, fichiers).
/// Renvoie aussi l'avertissement à montrer quand quelque chose a été coupé.
pub fn pull_layout(d: &PullDetail, now: i64) -> (Vec<LogLine>, Vec<Part>, Option<String>) {
    let p = &d.pull;
    let mut head = vec![plain(
        format!("#{} {}", p.number, p.title),
        Some(Tok::Heading),
    )];
    let mut state = format!("État     : {}", state_label(p));
    if let Some(when) = ago(now, p.closed.as_deref()) {
        state.push_str(&format!(" {when}"));
    }
    head.push(plain(state, None));
    head.push(plain(format!("Branches : {} → {}", p.head, p.base), None));
    let mut author = format!("Auteur   : {}", p.author);
    if let Some(when) = ago(now, p.created.as_deref()) {
        author.push_str(&format!(" · ouverte {when}"));
    }
    if let Some(when) = ago(now, p.updated.as_deref()) {
        author.push_str(&format!(" · mise à jour {when}"));
    }
    head.push(plain(author, None));
    if !p.labels.is_empty() {
        head.push(plain(format!("Étiquettes : {}", p.labels.join(", ")), None));
    }
    match p.mergeable {
        Some(true) => head.push(plain("Fusion   : possible", Some(Tok::Added))),
        Some(false) => head.push(plain("Fusion   : conflits à résoudre", Some(Tok::Removed))),
        None => {}
    }
    if let Some(c) = d.checks {
        let tok = match c {
            Status::Success => Some(Tok::Added),
            Status::Failure => Some(Tok::Removed),
            _ => None,
        };
        head.push(plain(
            format!("CI       : {} {}", check_symbol(c), check_word(c)),
            tok,
        ));
    }
    head.push(plain("", None));
    let comments = d.comments.as_ref().map_or(p.comments as usize, Vec::len);
    head.push(plain(
        format!(
            "{} · +{} −{} · {} · {} · {}",
            plural(
                p.changed_files as usize,
                "fichier modifié",
                "fichiers modifiés"
            ),
            p.additions,
            p.deletions,
            plural(d.commits.len(), "commit", "commits"),
            plural(d.reviews.len(), "revue", "revues"),
            plural(comments, "commentaire", "commentaires"),
        ),
        Some(Tok::Strong),
    ));
    head.push(plain("", None));
    if p.body.trim().is_empty() {
        head.push(plain("(pas de description)", Some(Tok::Meta)));
    } else {
        head.extend(prose(&p.body));
    }
    head.push(plain("", None));

    let mut parts = Vec::new();
    if !d.reviews.is_empty() {
        let mut lines = Vec::new();
        for r in &d.reviews {
            let (sym, verb) = review_verb(r);
            let mut title = format!("{sym} {} {verb}", r.author);
            if let Some(when) = ago(now, r.date.as_deref()) {
                title.push_str(&format!(" {when}"));
            }
            if r.dismissed {
                title.push_str(" (écartée)");
            } else if r.stale {
                title.push_str(" (sur un commit plus ancien)");
            }
            let tok = match r.state {
                _ if r.dismissed || r.stale => Some(Tok::Meta),
                ReviewState::Approved => Some(Tok::Added),
                ReviewState::ChangesRequested => Some(Tok::Removed),
                _ => Some(Tok::Strong),
            };
            lines.push(plain(title, tok));
            if !r.body.trim().is_empty() {
                lines.extend(prose(&r.body));
            }
            for c in &r.comments {
                lines.push(plain(format!("  {}", c.path), Some(Tok::Meta)));
                if let Some(ctx) = &c.context {
                    lines.push(plain(format!("  {ctx}"), Some(Tok::Hunk)));
                }
                lines.extend(prose(&c.body));
            }
            lines.push(plain("", None));
        }
        parts.push(Part {
            name: format!("Revues ({})", d.reviews.len()),
            status: reviews_status(&d.reviews),
            collapsed: false,
            lines,
        });
    }
    match &d.comments {
        Some(list) if !list.is_empty() => {
            let mut lines = Vec::new();
            for c in list {
                let mut title = c.author.clone();
                if let Some(when) = ago(now, c.date.as_deref()) {
                    title.push_str(&format!(" {when}"));
                }
                lines.push(plain(title, Some(Tok::Strong)));
                lines.extend(prose(&c.body));
                lines.push(plain("", None));
            }
            parts.push(Part {
                name: format!("Conversation ({})", list.len()),
                status: Status::Unknown,
                collapsed: false,
                lines,
            });
        }
        Some(_) => {}
        None if p.comments > 0 => parts.push(Part {
            name: format!("Conversation ({}, non affichée)", p.comments),
            status: Status::Skipped,
            collapsed: false,
            lines: vec![plain(
                "Le token n'a pas le droit read:issue, nécessaire pour lire la conversation. \
                 Un token qui l'a peut être mis dans la configuration du serveur (S, puis e).",
                Some(Tok::Meta),
            )],
        }),
        None => {}
    }
    if !d.commits.is_empty() {
        let mut lines: Vec<LogLine> = d
            .commits
            .iter()
            .map(|c| {
                let when = ago(now, c.date.as_deref()).unwrap_or_default();
                let mut l = plain(
                    format!("{} {} · {} {when}", c.short_sha(), c.summary(), c.author),
                    None,
                );
                l.runs = vec![crate::highlight::Run {
                    start: 0,
                    end: c.short_sha().len() as u32,
                    tok: Tok::Hunk,
                }];
                l
            })
            .collect();
        if d.commits_truncated {
            lines.push(plain(
                format!("(les {} premiers seulement)", d.commits.len()),
                Some(Tok::Meta),
            ));
        }
        parts.push(Part {
            name: format!("Commits ({})", d.commits.len()),
            status: Status::Unknown,
            collapsed: d.commits.len() >= COLLAPSE_COMMITS_FROM,
            lines,
        });
    }
    if !d.statuses.is_empty() {
        let lines = d
            .statuses
            .iter()
            .flat_map(|s| {
                let mut out = vec![plain(
                    format!(
                        "{} {} — {}",
                        check_symbol(s.status),
                        s.context,
                        if s.description.is_empty() {
                            check_word(s.status)
                        } else {
                            &s.description
                        }
                    ),
                    match s.status {
                        Status::Success => Some(Tok::Added),
                        Status::Failure => Some(Tok::Removed),
                        _ => None,
                    },
                )];
                if let Some(url) = &s.url {
                    out.push(plain(format!("  {url}"), Some(Tok::Link)));
                }
                out
            })
            .collect();
        parts.push(Part {
            name: format!("CI ({})", d.statuses.len()),
            status: d.checks.unwrap_or(Status::Unknown),
            collapsed: false,
            lines,
        });
    }
    let (files, line_cut) = diff_parts(&d.patch, &[]);
    if files.is_empty() {
        head.push(plain(
            "(aucun changement de fichier à afficher)",
            Some(Tok::Meta),
        ));
    }
    parts.extend(files);
    let notice = if d.patch_truncated || line_cut {
        Some("correctif tronqué : début affiché".to_owned())
    } else {
        None
    };
    (head, parts, notice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_core::{CheckStatus, Comment, Commit, ReviewComment};

    fn repo() -> Repo {
        Repo {
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn pr(n: u64) -> PullRequest {
        PullRequest {
            number: n,
            title: format!("feat: n°{n}"),
            body: String::new(),
            author: if n.is_multiple_of(2) { "ana" } else { "bob" }.into(),
            state: PullState::Open,
            draft: false,
            head: format!("feature/{n}"),
            base: "main".into(),
            head_sha: format!("{n:040x}"),
            labels: if n == 3 { vec!["docs".into()] } else { vec![] },
            comments: 0,
            additions: 1,
            deletions: 0,
            changed_files: 1,
            mergeable: Some(true),
            created: Some("2026-10-14T10:00:00Z".into()),
            updated: None,
            closed: None,
            url: None,
        }
    }

    fn page(range: std::ops::RangeInclusive<u64>, more: bool) -> PullPage {
        PullPage {
            pulls: range.rev().map(pr).collect(),
            has_more: more,
        }
    }

    fn req(state: PullFilter, page: u32) -> Request {
        Request::Pulls {
            repo: repo(),
            filter: state,
            page,
        }
    }

    #[test]
    fn liste_paginee_filtre_local_et_etats() {
        let (mut v, reqs) = PullsView::open(repo(), PullFilter::Open);
        assert_eq!(reqs, [req(PullFilter::Open, 1)]);
        assert!(v.loading);
        assert!(
            v.apply_page(&repo(), PullFilter::Open, 1, Ok(page(11..=60, true)))
                .is_empty()
        );
        assert_eq!(v.pulls.len(), 50);
        // Vers la fin : la page 2 est demandée, une seule fois
        let asked = v.move_selection(46);
        assert_eq!(asked, [req(PullFilter::Open, 2)]);
        assert!(v.move_selection(1).is_empty(), "déjà en route");
        // Une réponse d'un autre état, ou d'une page inattendue : ignorée
        assert!(
            v.apply_page(&repo(), PullFilter::All, 2, Ok(page(1..=10, false)))
                .is_empty()
        );
        v.apply_page(&repo(), PullFilter::Open, 3, Ok(page(1..=10, false)));
        assert_eq!(v.pulls.len(), 50);
        v.apply_page(&repo(), PullFilter::Open, 2, Ok(page(1..=10, false)));
        assert_eq!((v.pulls.len(), v.has_more), (60, false));

        // Filtre local : numéro, étiquette, branche, mots en ET
        v.filter.start();
        "#3".chars().for_each(|c| {
            v.filter.type_char(c);
        });
        v.filter_changed();
        let n: Vec<u64> = v.visible().iter().map(|p| p.number).collect();
        assert!(
            n.contains(&3) && n.contains(&30) && !n.contains(&4),
            "{n:?}"
        );
        v.filter.clear();
        "docs feature/3".chars().for_each(|c| {
            v.filter.editing = true;
            v.filter.type_char(c);
        });
        assert_eq!(
            v.visible().iter().map(|p| p.number).collect::<Vec<_>>(),
            [3]
        );

        // La page 1 rechargée garde la même PR sélectionnée
        v.filter.clear();
        v.selected = v.visible().iter().position(|p| p.number == 20).unwrap();
        v.refresh();
        v.apply_page(&repo(), PullFilter::Open, 1, Ok(page(15..=64, true)));
        assert_eq!(v.selected_pull().map(|p| p.number), Some(20));

        // `s` : fermées, liste vidée et redemandée
        assert_eq!(v.cycle_state(), [req(PullFilter::Closed, 1)]);
        assert!(v.pulls.is_empty() && v.loading && v.selected == 0);
        v.cycle_state();
        assert_eq!(v.cycle_state(), [req(PullFilter::Open, 1)]);
        // Erreur : affichée, pas de suite demandée
        v.apply_page(
            &repo(),
            PullFilter::Open,
            1,
            Err(Error::Network("coupé".into())),
        );
        assert!(v.error.as_deref().unwrap().contains("coupé"));
    }

    fn detail() -> PullDetail {
        let mut p = pr(1);
        p.title = "feat: page de garde".into();
        p.body = "Ajoute une **page**.\n\n- un\n- deux\n".into();
        p.labels = vec!["docs".into(), "enhancement".into()];
        p.comments = 2;
        p.changed_files = 2;
        p.additions = 3;
        p.deletions = 1;
        PullDetail {
            pull: p,
            commits: vec![Commit {
                sha: "c1a2b3c4d5".into(),
                message: "feat: page de garde\n".into(),
                author: "smithy".into(),
                date: Some("2026-10-14T10:00:00Z".into()),
                url: None,
            }],
            commits_truncated: false,
            reviews: vec![
                Review {
                    author: "relecteur".into(),
                    state: ReviewState::ChangesRequested,
                    body: "Il manque un test.".into(),
                    date: None,
                    dismissed: true,
                    stale: false,
                    comments: vec![ReviewComment {
                        path: "src/lib.rs".into(),
                        body: "Et le dépassement ?".into(),
                        context: Some("+    a - b".into()),
                    }],
                },
                Review {
                    author: "relecteur".into(),
                    state: ReviewState::Approved,
                    body: "Ça me va.".into(),
                    date: None,
                    dismissed: false,
                    stale: false,
                    comments: vec![],
                },
            ],
            comments: Some(vec![Comment {
                author: "relecteur".into(),
                body: "Un avis".into(),
                date: None,
            }]),
            checks: Some(Status::Running),
            statuses: vec![CheckStatus {
                context: "ci/build".into(),
                status: Status::Success,
                description: "compilation".into(),
                url: None,
            }],
            patch: "diff --git a/garde.md b/garde.md\nnew file mode 100644\n--- /dev/null\n+++ b/garde.md\n@@ -0,0 +1 @@\n+x\ndiff --git a/docs/guide.md b/docs/manuel.md\nsimilarity index 68%\nrename from docs/guide.md\nrename to docs/manuel.md\n".into(),
            patch_truncated: false,
        }
    }

    fn texts(lines: &[LogLine]) -> Vec<String> {
        lines.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn mise_en_forme_d_une_pr() {
        let (head, parts, notice) = pull_layout(&detail(), timeago::now_epoch());
        let head = texts(&head);
        assert_eq!(head[0], "#1 feat: page de garde");
        assert!(head.contains(&"État     : ouverte".to_owned()), "{head:?}");
        assert!(head.contains(&"Branches : feature/1 → main".to_owned()));
        assert!(head.contains(&"Étiquettes : docs, enhancement".to_owned()));
        assert!(head.contains(&"Fusion   : possible".to_owned()));
        assert!(head.contains(&"CI       : ● en cours".to_owned()));
        assert!(head.contains(
            &"2 fichiers modifiés · +3 −1 · 1 commit · 2 revues · 1 commentaire".to_owned()
        ));
        // La description est mise en forme (Markdown) : plus d'astérisques, des puces
        assert!(head.iter().any(|l| l == "Ajoute une page."), "{head:?}");
        assert!(head.iter().any(|l| l.contains("un") && l.starts_with('•')));

        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Revues (2)",
                "Conversation (1)",
                "Commits (1)",
                "CI (1)",
                "garde.md  +1 −0",
                "docs/manuel.md  +0 −0"
            ]
        );
        // La demande de changements est écartée : l'approbation l'emporte
        assert_eq!(parts[0].status, Status::Success);
        let reviews = texts(&parts[0].lines);
        assert!(reviews[0].contains("demande des changements") && reviews[0].contains("(écartée)"));
        assert!(reviews.iter().any(|l| l == "  src/lib.rs"));
        assert!(reviews.iter().any(|l| l == "  +    a - b"));
        assert!(
            reviews
                .iter()
                .any(|l| l.starts_with("✔ relecteur a approuvé"))
        );
        assert!(texts(&parts[2].lines)[0].starts_with("c1a2b3c feat: page de garde · smithy"));
        assert_eq!(texts(&parts[3].lines), ["✔ ci/build — compilation"]);
        assert_eq!(parts[4].status, Status::Success, "fichier ajouté");
        assert_eq!(parts[5].status, Status::Pending, "fichier renommé");
        assert_eq!(notice, None);
    }

    #[test]
    fn conversation_illisible_brouillon_et_conflit() {
        let mut d = detail();
        d.comments = None;
        d.pull.draft = true;
        d.pull.mergeable = Some(false);
        d.reviews.clear();
        d.statuses.clear();
        d.checks = None;
        d.patch_truncated = true;
        let (head, parts, notice) = pull_layout(&d, timeago::now_epoch());
        let head = texts(&head);
        assert!(head.contains(&"État     : brouillon".to_owned()));
        assert!(head.contains(&"Fusion   : conflits à résoudre".to_owned()));
        assert!(!head.iter().any(|l| l.starts_with("CI")));
        // Le nombre de commentaires vient de la PR quand la conversation est illisible
        assert!(head.iter().any(|l| l.ends_with("2 commentaires")));
        assert_eq!(parts[0].name, "Conversation (2, non affichée)");
        assert!(texts(&parts[0].lines)[0].contains("read:issue"));
        assert_eq!(notice.as_deref(), Some("correctif tronqué : début affiché"));
        // Une PR fusionnée sans description ni fichier
        let mut d = detail();
        d.pull.state = PullState::Merged;
        d.pull.mergeable = None;
        d.pull.body.clear();
        d.pull.closed = Some("2026-10-14T10:00:00Z".into());
        d.patch.clear();
        let (head, _, _) = pull_layout(&d, timeago::now_epoch());
        let head = texts(&head);
        assert!(head.iter().any(|l| l.starts_with("État     : fusionnée ")));
        assert!(!head.iter().any(|l| l.starts_with("Fusion")));
        assert!(head.contains(&"(pas de description)".to_owned()));
        assert!(head.contains(&"(aucun changement de fichier à afficher)".to_owned()));
    }

    #[test]
    fn detail_ignore_les_reponses_d_une_autre_pr() {
        let (mut v, reqs) = PullView::open(repo(), 1, "titre".into());
        assert_eq!(
            reqs,
            [Request::PullDetail {
                repo: repo(),
                number: 1
            }]
        );
        v.apply_detail(&repo(), 2, Ok(detail()));
        assert!(v.head.is_none() && matches!(v.text.state, LogState::Loading));
        v.apply_detail(&repo(), 1, Ok(detail()));
        assert_eq!(v.head.as_deref(), Some("feature/1"));
        assert_eq!(v.title, "feat: page de garde");
        assert!(matches!(v.text.state, LogState::Ready(_)));
        v.apply_detail(&repo(), 1, Err(Error::NotFound("#1".into())));
        assert!(matches!(v.text.state, LogState::Failed(_)));
    }
}
