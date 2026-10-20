//! Volet de texte plein écran (`TextView`) : sections par étape, défilement et retour à la
//! ligne. Il sert à la sortie d'un job (`LogView`) comme au contenu d'un fichier de l'explorateur.
//!
//! Le défilement se compte en **lignes d'écran** et non en lignes de log : une seule ligne
//! (un blob JSON de 5000 caractères) peut dépasser la hauteur du terminal une fois repliée,
//! et il faut pouvoir la lire jusqu'au bout. Les lignes d'une étape repliée ne comptent pour
//! aucune ligne d'écran. Aucune dépendance au terminal : la largeur et la hauteur utiles sont
//! passées par l'appelant.

use std::cell::RefCell;

use smithy_core::{JobLog, Pipeline, Repo, Status};

use crate::{
    diff::{DiffInfo, DiffKind},
    highlight::Run,
    textsearch::{find_ranges, smart_case},
    wrap,
};

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
    /// Étape à laquelle la ligne appartient (son titre compris), si elle en a une.
    pub step: Option<usize>,
    /// Couleurs de la coloration syntaxique (octets de `text`) ; vide pour un log.
    pub runs: Vec<Run>,
    /// Texte formaté (Markdown) : début des lignes d'écran qui suivent la première (retrait
    /// d'une puce, barre d'une citation), et retour à la ligne par mots plutôt qu'au caractère.
    pub hang: String,
    pub words: bool,
    /// Ligne d'un correctif : numéros, mots modifiés (voir `diff`).
    pub diff: Option<DiffInfo>,
}

/// Une section à afficher : son titre (coloré selon `status`), ses lignes et si elle démarre pliée.
pub struct Part {
    pub name: String,
    pub status: Status,
    pub lines: Vec<LogLine>,
    pub collapsed: bool,
}

impl LogLine {
    /// Une ligne de texte sans couleur ni section.
    pub fn text(text: String) -> Self {
        Self {
            kind: LineKind::Text,
            text,
            step: None,
            runs: Vec::new(),
            hang: String::new(),
            words: false,
            diff: None,
        }
    }

    fn hang_width(&self) -> usize {
        unicode_width::UnicodeWidthStr::width(self.hang.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum LogState {
    Loading,
    Ready(Vec<LogLine>),
    Failed(String),
}

/// Une ligne d'écran : un morceau de ligne de log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row<'a> {
    pub kind: LineKind,
    pub text: &'a str,
    /// Octet de `text` dans sa ligne (non nul sur la suite d'une ligne repliée) et les couleurs
    /// de **toute** la ligne : à découper avec `run_slices`.
    pub offset: usize,
    pub runs: &'a [Run],
    /// À afficher devant le texte quand `continuation` (voir `LogLine::hang`).
    pub hang: &'a str,
    /// La ligne dont la ligne d'écran fait partie : son numéro et son texte entier (pour la
    /// recherche, dont une occurrence peut être à cheval sur deux lignes d'écran).
    pub line_no: usize,
    pub line: &'a str,
    /// Suite d'une ligne repliée (et non début d'une ligne de log).
    pub continuation: bool,
    /// Ligne d'un correctif : l'affichage y ajoute la gouttière et le fond.
    pub diff: Option<&'a DiffInfo>,
    /// Titre de la section courante (celle que `Espace` plie ou déplie).
    pub current: bool,
}

impl Default for TextView {
    fn default() -> Self {
        Self::new()
    }
}

/// Vue plein écran de la sortie d'un job : un volet de texte et le job qu'il montre.
#[derive(Debug, Clone)]
pub struct LogView {
    pub repo: Repo,
    pub pipeline: Pipeline,
    view: TextView,
}

impl LogView {
    pub fn new(repo: Repo, pipeline: Pipeline) -> Self {
        Self {
            repo,
            pipeline,
            view: TextView::new(),
        }
    }
}

impl std::ops::Deref for LogView {
    type Target = TextView;
    fn deref(&self) -> &TextView {
        &self.view
    }
}

impl std::ops::DerefMut for LogView {
    fn deref_mut(&mut self) -> &mut TextView {
        &mut self.view
    }
}

/// Où se placer à l'ouverture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Start {
    /// Sur cette ligne de log (le titre de la première étape en échec).
    Line(usize),
    End,
}

/// Une étape, pliable.
#[derive(Debug, Clone)]
struct Section {
    name: String,
    /// Indice de son titre dans les lignes.
    header: usize,
    /// Nombre de lignes de sortie (titre exclu).
    len: usize,
    collapsed: bool,
}

impl Section {
    fn header_text(&self) -> String {
        if self.collapsed {
            let n = self.len;
            format!(
                "▸ {} ({n} ligne{})",
                self.name,
                if n > 1 { "s" } else { "" }
            )
        } else {
            format!("▾ {}", self.name)
        }
    }
}

/// Comme sur le web : les étapes réussies ou ignorées sont repliées, celles qui échouent ou
/// tournent encore restent ouvertes.
fn default_collapsed(status: Status) -> bool {
    matches!(status, Status::Success | Status::Skipped)
}

/// Début de chaque ligne de log, en lignes d'écran (`starts[i]`), plus le total à la fin.
/// Mis en cache : le recalcul coûte O(lignes), pas question de le refaire à chaque image.
#[derive(Debug, Clone)]
struct RowIndex {
    /// 0 quand le repli est désactivé : la largeur n'a alors aucun effet.
    width: usize,
    starts: Vec<usize>,
}

/// Colonnes de la gouttière : deux numéros de la largeur du plus grand, et une séparation.
fn gutter_of(lines: &[LogLine]) -> usize {
    let max = lines
        .iter()
        .filter_map(|l| l.diff.as_ref())
        .map(|d| d.old.max(d.new))
        .max();
    max.map_or(0, |m| 2 * (m.max(1).ilog10() as usize + 1) + 2)
}

/// Empreinte d'un texte ligne à ligne (voir `TextView::fingerprint`).
pub fn fingerprint_of<'a>(lines: impl IntoIterator<Item = &'a String>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    for l in lines {
        l.hash(&mut h);
    }
    h.finish()
}

/// Une occurrence de la recherche : ligne de texte et octets dans cette ligne.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Recherche validée : la requête, la casse retenue et toutes les occurrences.
#[derive(Debug, Clone)]
struct Search {
    query: String,
    case_sensitive: bool,
    matches: Vec<Match>,
    /// Occurrence atteinte avec `n` / `N` ; `None` tant qu'on n'a pas bougé.
    current: Option<usize>,
}

/// Volet de texte : des lignes, éventuellement groupées en sections, et leur défilement.
#[derive(Debug, Clone)]
pub struct TextView {
    pub state: LogState,
    /// Première ligne d'écran affichée.
    pub scroll: usize,
    /// Réserve à afficher à côté du texte (ex. version du serveur non vérifiée, fichier tronqué).
    pub notice: Option<String>,
    /// Retour à la ligne actif.
    pub wrap: bool,
    sections: Vec<Section>,
    /// Saisie de la recherche en cours (`/`), pas encore validée.
    pub prompt: Option<String>,
    search: Option<Search>,
    /// Section choisie avec les flèches, quand elle ne peut pas être la ligne du haut (tout tient à
    /// l'écran : on ne défile pas, mais on doit pouvoir atteindre chaque section). `None` :
    /// la section courante est celle de la ligne du haut.
    selected: Option<usize>,
    index: RefCell<Option<RowIndex>>,
    /// Colonnes de la gouttière des numéros de ligne d'un correctif (0 : aucune). Elle prend de
    /// la largeur au texte des lignes de correctif, pas aux autres.
    pub gutter: usize,
    /// Empreinte du correctif affiché : une coloration n'est appliquée qu'à celui-là.
    pub diff_key: Option<u64>,
}

impl TextView {
    pub fn new() -> Self {
        Self {
            state: LogState::Loading,
            scroll: 0,
            notice: None,
            wrap: false,
            sections: Vec::new(),
            prompt: None,
            search: None,
            selected: None,
            index: RefCell::new(None),
            gutter: 0,
            diff_key: None,
        }
    }

    /// Texte sans section (un fichier) : les lignes telles quelles.
    pub fn set_plain(&mut self, lines: Vec<String>) {
        self.sections.clear();
        self.selected = None;
        self.state = LogState::Ready(
            lines
                .into_iter()
                .map(|text| LogLine {
                    kind: LineKind::Text,
                    text,
                    step: None,
                    runs: Vec::new(),
                    hang: String::new(),
                    words: false,
                    diff: None,
                })
                .collect(),
        );
        *self.index.borrow_mut() = None;
        self.refresh_search();
    }

    // --- recherche

    /// `/` : ouvre la saisie, préremplie avec la dernière requête.
    pub fn begin_search(&mut self) {
        self.prompt = Some(
            self.search
                .as_ref()
                .map_or_else(String::new, |s| s.query.clone()),
        );
    }

    pub fn prompt_type(&mut self, c: char) {
        if let Some(p) = self.prompt.as_mut() {
            p.push(c);
        }
    }

    pub fn prompt_backspace(&mut self) {
        if let Some(p) = self.prompt.as_mut() {
            p.pop();
        }
    }

    pub fn prompt_cancel(&mut self) {
        self.prompt = None;
    }

    /// Entrée : lance la recherche et va à la première occurrence à partir du haut de l'écran.
    /// Une saisie vide efface la recherche.
    pub fn prompt_commit(&mut self, width: usize, height: usize) {
        let Some(query) = self.prompt.take() else {
            return;
        };
        if query.is_empty() {
            self.search = None;
            return;
        }
        let case_sensitive = smart_case(&query);
        let matches = self.collect_matches(&query, case_sensitive);
        self.search = Some(Search {
            query,
            case_sensitive,
            matches,
            current: None,
        });
        self.step_match(1, width, height);
    }

    fn collect_matches(&self, query: &str, case_sensitive: bool) -> Vec<Match> {
        self.lines()
            .iter()
            .enumerate()
            // Les titres d'étape sont de l'habillage (flèche, nombre de lignes) : pas du texte.
            .filter(|(_, l)| l.kind == LineKind::Text)
            .flat_map(|(line, l)| {
                find_ranges(&l.text, query, case_sensitive)
                    .into_iter()
                    .map(move |(start, end)| Match { line, start, end })
            })
            .collect()
    }

    /// Le contenu a changé (log suivi en direct, fichier rechargé) : on recompte, en gardant
    /// l'occurrence courante si elle existe encore.
    fn refresh_search(&mut self) {
        let Some(s) = self.search.as_ref() else {
            return;
        };
        let (query, cs) = (s.query.clone(), s.case_sensitive);
        let previous = s.current.and_then(|i| s.matches.get(i).copied());
        let matches = self.collect_matches(&query, cs);
        let current = previous.and_then(|p| matches.iter().position(|m| *m == p));
        self.search = Some(Search {
            query,
            case_sensitive: cs,
            matches,
            current,
        });
    }

    /// `n` (`1`) et `N` (`-1`) : occurrence suivante ou précédente, en boucle. La première fois,
    /// on part du haut de l'écran. Une occurrence dans une étape pliée la déplie.
    pub fn step_match(&mut self, delta: isize, width: usize, height: usize) {
        let top_line = self.line_of_row(width, self.scroll);
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let n = s.matches.len();
        if n == 0 {
            s.current = None;
            return;
        }
        let next = match s.current {
            Some(i) => (i as isize + delta).rem_euclid(n as isize) as usize,
            None if delta >= 0 => s
                .matches
                .iter()
                .position(|m| m.line >= top_line)
                .unwrap_or(0),
            None => s
                .matches
                .iter()
                .rposition(|m| m.line < top_line)
                .unwrap_or(n - 1),
        };
        s.current = Some(next);
        let m = s.matches[next];
        self.reveal_match(m, width, height);
    }

    fn reveal_match(&mut self, m: Match, width: usize, height: usize) {
        let step = self
            .lines()
            .get(m.line)
            .filter(|l| self.hidden(l))
            .and_then(|l| l.step);
        if let Some(step) = step {
            self.sections[step].collapsed = false;
            self.refresh_headers();
        }
        // Ligne d'écran de l'occurrence : celle du morceau replié qui la contient.
        let mut row = self.row_of_line(width, m.line);
        if self.wrap
            && let Some(line) = self.lines().get(m.line)
        {
            let width = self.text_width(line, width);
            let parts = wrap::layout(&line.text, width, line.hang_width(), line.words);
            row += parts.iter().rposition(|p| p.0 <= m.start).unwrap_or(0);
        }
        // Un peu de contexte au-dessus de l'occurrence.
        let max = self.max_scroll(width, height);
        self.scroll_to_row(row.saturating_sub(height / 3), max);
    }

    /// Esc : efface la recherche (et sa mise en évidence). `false` s'il n'y en avait pas.
    pub fn clear_search(&mut self) -> bool {
        self.prompt = None;
        self.search.take().is_some()
    }

    pub fn has_search(&self) -> bool {
        self.search.is_some()
    }

    /// Pour l'affichage : `(requête, rang de l'occurrence courante à partir de 1 ou 0, total)`.
    pub fn search_status(&self) -> Option<(&str, usize, usize)> {
        self.search.as_ref().map(|s| {
            (
                s.query.as_str(),
                s.current.map_or(0, |i| i + 1),
                s.matches.len(),
            )
        })
    }

    /// Occurrences de la recherche dans le texte d'une ligne (pour la surbrillance).
    pub fn line_matches(&self, text: &str) -> Vec<(usize, usize)> {
        self.search
            .as_ref()
            .map_or_else(Vec::new, |s| find_ranges(text, &s.query, s.case_sensitive))
    }

    /// L'occurrence courante, à mettre en avant.
    pub fn current_match(&self) -> Option<Match> {
        self.search
            .as_ref()
            .and_then(|s| s.current.and_then(|i| s.matches.get(i).copied()))
    }

    /// Empreinte du texte affiché : une coloration calculée pour un autre texte (fichier rechargé
    /// entre-temps) ne doit pas être appliquée.
    pub fn fingerprint(&self) -> u64 {
        match &self.state {
            LogState::Ready(lines) => fingerprint_of(lines.iter().map(|l| &l.text)),
            _ => fingerprint_of(std::iter::empty::<&String>()),
        }
    }

    /// Applique la coloration (une liste de régions par ligne). Refusée si le nombre de lignes
    /// ne correspond pas ou si le texte n'est plus celui pour lequel elle a été calculée.
    pub fn set_styles(&mut self, fingerprint: u64, styles: Vec<Vec<Run>>) -> bool {
        if self.fingerprint() != fingerprint {
            return false;
        }
        let LogState::Ready(lines) = &mut self.state else {
            return false;
        };
        if lines.len() != styles.len() {
            return false;
        }
        for (line, runs) in lines.iter_mut().zip(styles) {
            line.runs = runs;
        }
        true
    }

    /// Applique la coloration d'un correctif (par fichier, par ligne : voir `diff::highlight_patch`).
    /// Refusée si ce n'est plus le correctif pour lequel elle a été calculée.
    pub fn set_diff_styles(&mut self, key: u64, styles: &[Vec<Vec<Run>>]) -> bool {
        if self.diff_key != Some(key) {
            return false;
        }
        let LogState::Ready(lines) = &mut self.state else {
            return false;
        };
        for line in lines {
            let Some(d) = &line.diff else { continue };
            if matches!(d.kind, DiffKind::Hunk | DiffKind::Meta) {
                continue;
            }
            if let Some(runs) = styles
                .get(d.file as usize)
                .and_then(|f| f.get(d.idx as usize))
            {
                line.runs.clone_from(runs);
            }
        }
        true
    }

    /// Largeur du texte d'une ligne : celle de l'écran, moins la gouttière d'une ligne de correctif.
    fn text_width(&self, line: &LogLine, width: usize) -> usize {
        if line.diff.is_some() {
            width.saturating_sub(self.gutter).max(1)
        } else {
            width
        }
    }

    /// Remplace le contenu. L'état plié/déplié des étapes déjà vues est conservé ; une étape
    /// vue pour la première fois prend le réglage par défaut selon son statut (donc une étape
    /// ouverte parce qu'elle tournait le reste après sa fin). Le défilement est à recaler.
    pub fn set_log(&mut self, log: &JobLog) {
        let preface = if log.steps.is_empty() {
            vec![LogLine::text(
                "(aucune étape : le job n'a pas démarré)".to_owned(),
            )]
        } else {
            Vec::new()
        };
        let parts = log
            .steps
            .iter()
            .map(|step| Part {
                name: step.name.clone(),
                status: step.status,
                lines: step.lines.iter().cloned().map(LogLine::text).collect(),
                collapsed: default_collapsed(step.status),
            })
            .collect();
        self.set_sectioned(preface, parts);
    }

    /// Remplace le contenu par des lignes libres suivies de sections pliables. L'état plié/déplié
    /// des sections déjà vues (par indice) est conservé ; une nouvelle prend `Part::collapsed`.
    pub fn set_sectioned(&mut self, preface: Vec<LogLine>, parts: Vec<Part>) {
        let previous: Vec<bool> = self.sections.iter().map(|s| s.collapsed).collect();
        let mut lines = preface;
        let mut sections = Vec::new();
        for (i, part) in parts.into_iter().enumerate() {
            sections.push(Section {
                name: part.name,
                header: lines.len(),
                len: part.lines.len(),
                collapsed: previous.get(i).copied().unwrap_or(part.collapsed),
            });
            lines.push(LogLine {
                kind: LineKind::Step(part.status),
                text: String::new(), // rempli par `refresh_headers`
                step: Some(i),
                runs: Vec::new(),
                hang: String::new(),
                words: false,
                diff: None,
            });
            lines.extend(part.lines.into_iter().map(|mut l| {
                l.step = Some(i);
                l
            }));
        }
        self.sections = sections;
        self.gutter = gutter_of(&lines);
        self.state = LogState::Ready(lines);
        self.refresh_headers();
        self.refresh_search();
    }

    /// Remplace le contenu par des lignes sans étape, donc jamais repliées (fichier, Markdown).
    pub fn set_lines(&mut self, lines: Vec<LogLine>) {
        self.sections.clear();
        self.gutter = gutter_of(&lines);
        self.state = LogState::Ready(lines);
        *self.index.borrow_mut() = None;
        self.refresh_search();
    }

    /// Réécrit les titres (flèche, nombre de lignes cachées) et invalide l'index d'écran.
    fn refresh_headers(&mut self) {
        if let LogState::Ready(lines) = &mut self.state {
            for s in &self.sections {
                lines[s.header].text = s.header_text();
            }
        }
        *self.index.borrow_mut() = None;
    }

    fn lines(&self) -> &[LogLine] {
        match &self.state {
            LogState::Ready(lines) => lines,
            _ => &[],
        }
    }

    /// Les lignes de sortie d'une étape repliée n'occupent aucune ligne d'écran.
    fn hidden(&self, line: &LogLine) -> bool {
        line.kind == LineKind::Text
            && line
                .step
                .and_then(|i| self.sections.get(i))
                .is_some_and(|s| s.collapsed)
    }

    /// Exécute `f` sur l'index des lignes d'écran pour cette largeur, en le (re)construisant au besoin.
    fn with_index<R>(&self, width: usize, f: impl FnOnce(&RowIndex) -> R) -> R {
        let key = if self.wrap { width.max(1) } else { 0 };
        let mut cache = self.index.borrow_mut();
        if cache.as_ref().is_none_or(|i| i.width != key) {
            let mut starts = Vec::with_capacity(self.lines().len() + 1);
            let mut total = 0;
            for l in self.lines() {
                starts.push(total);
                total += if self.hidden(l) {
                    0
                } else if self.wrap {
                    wrap::count(&l.text, self.text_width(l, key), l.hang_width(), l.words)
                } else {
                    1
                };
            }
            starts.push(total);
            *cache = Some(RowIndex { width: key, starts });
        }
        f(cache.as_ref().expect("index construit ci-dessus"))
    }

    pub fn total_rows(&self, width: usize) -> usize {
        self.with_index(width, |i| *i.starts.last().unwrap_or(&0))
    }

    /// Plus grand défilement possible : la dernière ligne d'écran en bas de la fenêtre.
    pub fn max_scroll(&self, width: usize, height: usize) -> usize {
        self.total_rows(width).saturating_sub(height.max(1))
    }

    /// Première ligne d'écran de la ligne de log `line`.
    pub fn row_of_line(&self, width: usize, line: usize) -> usize {
        self.with_index(width, |i| i.starts.get(line).copied().unwrap_or(0))
    }

    /// Ligne de log qui contient la ligne d'écran `row`. Les lignes cachées (0 ligne d'écran)
    /// partagent leur début avec la suivante : c'est toujours la dernière à ce début qui est visible.
    pub fn line_of_row(&self, width: usize, row: usize) -> usize {
        self.with_index(width, |i| {
            // `starts` finit par le total (pas une ligne) : on borne à la dernière vraie ligne.
            let last = i.starts.len().saturating_sub(2);
            i.starts
                .partition_point(|&s| s <= row)
                .saturating_sub(1)
                .min(last)
        })
    }

    /// Section courante, celle que Espace plie ou déplie : la choisie avec les flèches, sinon l'étape
    /// de la ligne en haut de l'écran.
    pub fn current_step(&self, width: usize) -> Option<usize> {
        self.selected
            .filter(|&i| i < self.sections.len())
            .or_else(|| {
                self.lines()
                    .get(self.line_of_row(width, self.scroll))
                    .and_then(|l| l.step)
            })
    }

    /// Défilement demandé par l'utilisateur : la section courante redevient celle du haut.
    pub fn scroll_to_row(&mut self, row: usize, max: usize) {
        self.scroll = row.min(max);
        self.selected = None;
    }

    /// Bascule le retour à la ligne en gardant la même ligne de log en haut de l'écran.
    pub fn toggle_wrap(&mut self, width: usize, height: usize) {
        let top_line = self.line_of_row(width, self.scroll);
        self.wrap = !self.wrap;
        self.scroll = self
            .row_of_line(width, top_line)
            .min(self.max_scroll(width, height));
    }

    /// Plie ou déplie la section courante ; son titre reste visible.
    pub fn toggle_section(&mut self, width: usize, height: usize) {
        let Some(step) = self.current_step(width) else {
            return;
        };
        self.sections[step].collapsed ^= true;
        self.selected = Some(step);
        self.refresh_headers();
        self.reveal_section(width, height, step);
    }

    /// Clic sur la ligne d'écran `row` de la fenêtre (0 = celle du haut) : sur le titre d'une
    /// section, la plie ou la déplie ; ailleurs, rien.
    pub fn click_row(&mut self, row: usize, width: usize, height: usize) {
        let Some(line) = self.window(width, height).get(row).map(|r| r.line_no) else {
            return;
        };
        let Some(step) = self
            .lines()
            .get(line)
            .filter(|l| matches!(l.kind, LineKind::Step(_)))
            .and_then(|l| l.step)
        else {
            return;
        };
        self.selected = Some(step);
        self.toggle_section(width, height);
    }

    /// Tout déplier, ou tout plier si tout est déjà déplié. La section courante reste visible.
    pub fn toggle_all(&mut self, width: usize, height: usize) {
        if self.sections.is_empty() {
            return;
        }
        let current = self.current_step(width);
        let collapse = self.sections.iter().all(|s| !s.collapsed);
        for s in &mut self.sections {
            s.collapsed = collapse;
        }
        self.selected = current;
        self.refresh_headers();
        match current {
            Some(step) => self.reveal_section(width, height, step),
            None => self.scroll = 0,
        }
    }

    /// Flèches droite / gauche : section suivante ou précédente (bornées aux extrémités).
    pub fn select_section(&mut self, delta: isize, width: usize, height: usize) {
        if self.sections.is_empty() {
            return;
        }
        let current = self.current_step(width).unwrap_or(0);
        let next = current
            .saturating_add_signed(delta)
            .min(self.sections.len() - 1);
        self.selected = Some(next);
        self.reveal_section(width, height, next);
    }

    /// Fait défiler le minimum pour que le titre de la section soit visible : s'il l'est déjà,
    /// rien ne bouge ; sinon il vient en haut.
    fn reveal_section(&mut self, width: usize, height: usize, step: usize) {
        let row = self.row_of_line(width, self.sections[step].header);
        let max = self.max_scroll(width, height);
        if row < self.scroll || row >= self.scroll + height.max(1) {
            self.scroll = row.min(max);
        } else {
            self.scroll = self.scroll.min(max);
        }
    }

    /// Où se placer à l'ouverture : sur la première étape en échec (c'est là qu'on cherche),
    /// sinon en bas si le job tourne encore, sinon en haut (`None`).
    pub fn initial_start(&self, live: bool) -> Option<Start> {
        self.lines()
            .iter()
            .position(|l| matches!(l.kind, LineKind::Step(Status::Failure)))
            .map(Start::Line)
            .or(live.then_some(Start::End))
    }

    /// Les lignes d'écran visibles : de `scroll` sur `height` lignes.
    pub fn window<'a>(&'a self, width: usize, height: usize) -> Vec<Row<'a>> {
        if height == 0 {
            return Vec::new();
        }
        let lines = self.lines();
        let first_line = self.line_of_row(width, self.scroll);
        let first_row = self.row_of_line(width, first_line);
        let mut skip = self.scroll.saturating_sub(first_row);
        let current = self.current_step(width);
        let mut out = Vec::with_capacity(height);
        for (line_no, line) in lines
            .iter()
            .enumerate()
            .skip(first_line)
            .filter(|(_, l)| !self.hidden(l))
        {
            let is_current = matches!(line.kind, LineKind::Step(_)) && line.step == current;
            let mut push = |text: &'a str, offset: usize, continuation| {
                out.push(Row {
                    kind: line.kind,
                    text,
                    offset,
                    runs: &line.runs,
                    hang: &line.hang,
                    line_no,
                    line: &line.text,
                    continuation,
                    diff: line.diff.as_ref(),
                    current: is_current,
                });
                out.len() == height
            };
            if self.wrap {
                let width = self.text_width(line, width);
                let parts = wrap::layout(&line.text, width, line.hang_width(), line.words);
                for (n, (start, end)) in parts.into_iter().enumerate() {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    if push(&line.text[start..end], start, n > 0) {
                        return out;
                    }
                }
            } else if push(&line.text, 0, false) {
                return out;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(texts: &[&str]) -> LogView {
        let pipeline = Pipeline {
            id: 1,
            number: 1,
            job: "j".into(),
            workflow: "w.yml".into(),
            title: "t".into(),
            branch: "main".into(),
            commit: "abc".into(),
            status: Status::Success,
            url: None,
        };
        let mut v = LogView::new(
            Repo {
                owner: "o".into(),
                name: "r".into(),
            },
            pipeline,
        );
        v.set_lines(
            texts
                .iter()
                .map(|t| LogLine {
                    kind: LineKind::Text,
                    text: (*t).to_owned(),
                    step: None,
                    runs: Vec::new(),
                    hang: String::new(),
                    words: false,
                    diff: None,
                })
                .collect(),
        );
        v
    }

    fn texts<'a>(rows: &[Row<'a>]) -> Vec<&'a str> {
        rows.iter().map(|r| r.text).collect()
    }

    #[test]
    fn sans_repli_une_ligne_de_log_par_ligne_d_ecran() {
        let v = view(&["aaaaaaaaaa", "b", "c"]);
        assert_eq!(v.total_rows(4), 3, "la largeur n'a aucun effet");
        assert_eq!(texts(&v.window(4, 10)), ["aaaaaaaaaa", "b", "c"]);
        assert!(v.window(4, 10).iter().all(|r| !r.continuation));
    }

    #[test]
    fn repli_decoupe_et_marque_les_suites() {
        let mut v = view(&["abcdefgh", "xy"]);
        v.wrap = true;
        assert_eq!(v.total_rows(3), 3 + 1);
        let rows = v.window(3, 10);
        assert_eq!(texts(&rows), ["abc", "def", "gh", "xy"]);
        let cont: Vec<_> = rows.iter().map(|r| r.continuation).collect();
        assert_eq!(cont, [false, true, true, false]);
    }

    #[test]
    fn une_ligne_plus_haute_que_l_ecran_se_lit_en_entier() {
        // 50 caractères sur 5 colonnes = 10 lignes d'écran, pour 4 de hauteur.
        let long: String = (0..50).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        let mut v = view(&[&long, "fin"]);
        v.wrap = true;
        assert_eq!(v.total_rows(5), 11);
        assert_eq!(v.max_scroll(5, 4), 7);
        let mut seen = String::new();
        for scroll in 0..=v.max_scroll(5, 4) {
            v.scroll = scroll;
            let w = v.window(5, 4);
            assert_eq!(w.len(), 4);
            if scroll % 4 == 0 {
                seen.push_str(&texts(&w).concat());
            }
        }
        // Les fenêtres 0 et 4 couvrent 8 lignes d'écran = 40 caractères au moins
        assert!(long.starts_with(&seen[..40]));
        // Tout en bas : la fin de la ligne et la ligne suivante, sans rien couper
        v.scroll = v.max_scroll(5, 4);
        assert_eq!(texts(&v.window(5, 4)).last(), Some(&"fin"));
        assert!(long.ends_with(texts(&v.window(5, 4))[2]));
    }

    #[test]
    fn defilement_a_mi_ligne() {
        let mut v = view(&["abcdefghi", "z"]);
        v.wrap = true;
        v.scroll = 1; // deuxième ligne d'écran de la première ligne de log
        assert_eq!(texts(&v.window(3, 10)), ["def", "ghi", "z"]);
        assert!(v.window(3, 10)[0].continuation);
        assert_eq!(v.line_of_row(3, 1), 0);
        assert_eq!(v.line_of_row(3, 3), 1);
    }

    #[test]
    fn correspondance_lignes_de_log_et_lignes_d_ecran() {
        let mut v = view(&["abcdef", "g", "hijkl"]);
        v.wrap = true;
        // starts = [0, 2, 3, 5]
        assert_eq!(v.row_of_line(3, 0), 0);
        assert_eq!(v.row_of_line(3, 1), 2);
        assert_eq!(v.row_of_line(3, 2), 3);
        assert_eq!(v.row_of_line(3, 99), 0, "ligne inconnue : le début");
        for row in 0..5 {
            let line = v.line_of_row(3, row);
            assert!(v.row_of_line(3, line) <= row);
        }
        assert_eq!(v.line_of_row(3, 99), 2, "au-delà de la fin : la dernière");
    }

    #[test]
    fn bascule_en_gardant_la_meme_ligne_en_haut() {
        let mut v = view(&["aaaaaa", "bbbbbb", "cccccc", "dddddd"]);
        v.scroll = 2; // sans repli : en haut, la ligne « cccccc » (4 lignes, 2 visibles)
        v.toggle_wrap(3, 2);
        assert!(v.wrap);
        // Avec repli à 3 colonnes chaque ligne vaut 2 lignes d'écran : « cccccc » commence en 4.
        assert_eq!(v.scroll, 4);
        assert_eq!(texts(&v.window(3, 2)), ["ccc", "ccc"]);
        v.toggle_wrap(3, 2);
        assert!(!v.wrap);
        assert_eq!(v.scroll, 2);
        assert_eq!(texts(&v.window(3, 2)), ["cccccc", "dddddd"]);
    }

    #[test]
    fn bascule_bornee_par_le_defilement_maximal() {
        // 4 lignes sur 4 de hauteur : rien à défiler sans repli, la position est ramenée à 0.
        let mut v = view(&["aaaaaa", "bbbbbb", "cccccc", "dddddd"]);
        v.scroll = 2;
        v.wrap = true;
        v.toggle_wrap(3, 4);
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn bascule_a_mi_ligne_revient_au_debut_de_la_ligne() {
        let mut v = view(&["abcdefghi", "z"]);
        v.wrap = true;
        v.scroll = 2; // dans la 3e ligne d'écran de la première ligne de log
        v.toggle_wrap(3, 10);
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn le_cache_suit_la_largeur_et_le_contenu() {
        let mut v = view(&["abcdef"]);
        v.wrap = true;
        assert_eq!(v.total_rows(3), 2);
        assert_eq!(v.total_rows(2), 3, "autre largeur : recalculé");
        assert_eq!(v.total_rows(6), 1);
        v.set_lines(vec![LogLine {
            kind: LineKind::Text,
            text: "abcdefghijkl".into(),
            step: None,
            runs: Vec::new(),
            hang: String::new(),
            words: false,
            diff: None,
        }]);
        assert_eq!(v.total_rows(6), 2, "autre contenu : recalculé");
    }

    #[test]
    fn contenu_vide_ou_en_chargement() {
        let mut v = view(&[]);
        assert_eq!(v.total_rows(10), 0);
        assert_eq!(v.max_scroll(10, 5), 0);
        assert!(v.window(10, 5).is_empty());
        v.state = LogState::Loading;
        assert!(v.window(10, 5).is_empty());
        v.wrap = true;
        assert_eq!(v.line_of_row(10, 3), 0);
        v.toggle_wrap(10, 5);
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn largeur_et_hauteur_degenerees() {
        let mut v = view(&["日本語", "a"]);
        v.wrap = true;
        assert_eq!(v.total_rows(0), 4);
        assert_eq!(v.window(0, 0), []);
        assert_eq!(v.window(1, 1).len(), 1);
    }

    // --- sections (une par étape)

    use smithy_core::StepLog;

    fn joblog(steps: &[(&str, Status, usize)]) -> JobLog {
        JobLog {
            steps: steps
                .iter()
                .map(|(name, status, n)| StepLog {
                    name: (*name).to_owned(),
                    status: *status,
                    lines: (1..=*n).map(|i| format!("{name} {i}")).collect(),
                })
                .collect(),
            notice: None,
        }
    }

    fn sectioned(steps: &[(&str, Status, usize)]) -> LogView {
        let mut v = view(&[]);
        v.set_log(&joblog(steps));
        v
    }

    const W: usize = 80;

    #[test]
    fn par_defaut_les_etapes_reussies_sont_repliees() {
        let v = sectioned(&[
            ("Set up job", Status::Success, 9),
            ("Tests", Status::Failure, 2),
            ("Build", Status::Running, 3),
            ("Lint", Status::Skipped, 1),
            ("Cleanup", Status::Cancelled, 2),
        ]);
        let t = texts(&v.window(W, 50));
        assert_eq!(
            t,
            [
                "▸ Set up job (9 lignes)",
                "▾ Tests",
                "Tests 1",
                "Tests 2",
                "▾ Build",
                "Build 1",
                "Build 2",
                "Build 3",
                "▸ Lint (1 ligne)",
                "▾ Cleanup",
                "Cleanup 1",
                "Cleanup 2",
            ]
        );
    }

    #[test]
    fn les_lignes_repliees_ne_comptent_pour_aucune_ligne_d_ecran() {
        let v = sectioned(&[("a", Status::Success, 100), ("b", Status::Failure, 2)]);
        assert_eq!(
            v.total_rows(W),
            1 + 1 + 2,
            "100 lignes cachées : 0 ligne d'écran"
        );
        // Avec repli à 3 colonnes, les lignes cachées ne comptent toujours pas
        let mut w = sectioned(&[("a", Status::Success, 100), ("b", Status::Failure, 2)]);
        w.wrap = true;
        // « ▸ a (100 lignes) » (16 caractères) fait 6 lignes de 3 colonnes ; « ▾ b » et ses
        // deux lignes, 1 chacune. Les 100 lignes cachées ne comptent pas, le titre si.
        assert_eq!(w.total_rows(3), 6 + 1 + 1 + 1);
    }

    #[test]
    fn la_section_courante_est_celle_de_la_ligne_du_haut() {
        let mut v = sectioned(&[("a", Status::Failure, 3), ("b", Status::Failure, 3)]);
        assert_eq!(v.current_step(W), Some(0));
        v.scroll = 3; // dernière ligne de « a »
        assert_eq!(v.current_step(W), Some(0));
        v.scroll = 4; // titre de « b »
        assert_eq!(v.current_step(W), Some(1));
        v.scroll = 6;
        assert_eq!(v.current_step(W), Some(1));
    }

    #[test]
    fn seul_le_titre_de_la_section_courante_est_marque() {
        let mut v = sectioned(&[("a", Status::Failure, 2), ("b", Status::Failure, 2)]);
        v.scroll = 2; // « a 2 » en haut : la section courante est « a », son titre n'est pas visible
        let current: Vec<_> = v
            .window(W, 10)
            .iter()
            .map(|r| (r.text, r.current))
            .collect();
        assert_eq!(
            current,
            [
                ("a 2", false),
                ("▾ b", false),
                ("b 1", false),
                ("b 2", false)
            ]
        );
        v.scroll = 0;
        let marked: Vec<_> = v
            .window(W, 10)
            .iter()
            .filter(|r| r.current)
            .map(|r| r.text)
            .collect();
        assert_eq!(
            marked,
            ["▾ a"],
            "le titre de « a », section courante, est marqué"
        );
    }

    #[test]
    fn espace_deplie_et_replie_la_section_courante() {
        let mut v = sectioned(&[("a", Status::Success, 5), ("b", Status::Success, 5)]);
        assert_eq!(
            texts(&v.window(W, 10)),
            ["▸ a (5 lignes)", "▸ b (5 lignes)"]
        );
        v.scroll = 1; // le titre de « b » est en haut : c'est la section courante
        v.toggle_section(W, 3);
        assert_eq!(v.scroll, 1, "son titre reste en haut");
        assert_eq!(texts(&v.window(W, 3)), ["▾ b", "b 1", "b 2"]);
        v.toggle_section(W, 3);
        // Tout tient maintenant dans la fenêtre : le défilement est ramené dans les bornes.
        assert_eq!(v.total_rows(W), 2);
        assert_eq!(v.scroll, 0);
        assert_eq!(texts(&v.window(W, 3)), ["▸ a (5 lignes)", "▸ b (5 lignes)"]);
    }

    #[test]
    fn replier_depuis_l_interieur_ramene_au_titre() {
        let mut v = sectioned(&[("a", Status::Failure, 20), ("b", Status::Failure, 1)]);
        v.scroll = 12; // au milieu de « a »
        v.toggle_section(W, 5);
        assert_eq!(v.scroll, 0, "on retombe sur le titre de la section repliée");
        assert_eq!(texts(&v.window(W, 5)), ["▸ a (20 lignes)", "▾ b", "b 1"]);
    }

    #[test]
    fn tout_deplier_puis_tout_replier() {
        let mut v = sectioned(&[
            ("a", Status::Success, 2),
            ("b", Status::Failure, 2),
            ("c", Status::Success, 2),
        ]);
        v.scroll = 2; // « b 1 » en haut : la section courante est « b » (a: 1 ligne, b: 1 titre + 2)
        v.toggle_all(W, 4); // il en reste de repliées : tout déplier
        assert!(texts(&v.window(W, 20)).iter().all(|t| !t.starts_with('▸')));
        assert_eq!(v.total_rows(W), 9);
        assert!(
            texts(&v.window(W, 4)).contains(&"▾ b"),
            "la section courante reste visible"
        );
        assert_eq!(v.current_step(W), Some(1));
        v.toggle_all(W, 4); // tout est déplié : tout replier
        assert_eq!(
            texts(&v.window(W, 20)),
            ["▸ a (2 lignes)", "▸ b (2 lignes)", "▸ c (2 lignes)"]
        );
        assert_eq!(v.scroll, 0, "bornée par le nouveau maximum");
    }

    #[test]
    fn l_etat_survit_au_rafraichissement_et_les_nouvelles_etapes_suivent_le_defaut() {
        let mut v = sectioned(&[("a", Status::Success, 2), ("b", Status::Running, 2)]);
        // L'utilisateur déplie « a » (réussie) : elle le reste après un rafraîchissement.
        v.scroll = 0;
        v.toggle_section(W, 20);
        // « b » finit : elle était ouverte pendant qu'on la lisait, elle ne se referme pas sous nos yeux.
        // Une nouvelle étape « c » réussie arrive : repliée par défaut.
        v.set_log(&joblog(&[
            ("a", Status::Success, 2),
            ("b", Status::Success, 4),
            ("c", Status::Success, 7),
        ]));
        let t = texts(&v.window(W, 30));
        assert_eq!(
            t,
            [
                "▾ a",
                "a 1",
                "a 2",
                "▾ b",
                "b 1",
                "b 2",
                "b 3",
                "b 4",
                "▸ c (7 lignes)"
            ]
        );
    }

    #[test]
    fn un_rafraichissement_garde_les_titres_a_jour() {
        let mut v = sectioned(&[("a", Status::Success, 2)]);
        assert_eq!(texts(&v.window(W, 5)), ["▸ a (2 lignes)"]);
        v.set_log(&joblog(&[("a", Status::Success, 9)]));
        assert_eq!(
            texts(&v.window(W, 5)),
            ["▸ a (9 lignes)"],
            "le nombre de lignes suit"
        );
    }

    #[test]
    fn job_sans_etape_et_vue_sans_section() {
        let mut v = view(&[]);
        v.set_log(&JobLog::default());
        assert!(texts(&v.window(W, 5))[0].contains("pas démarré"));
        // Rien à plier : aucune action ne doit paniquer ni bouger.
        v.toggle_section(W, 5);
        v.toggle_all(W, 5);
        assert_eq!(v.scroll, 0);
        let mut plain = view(&["a", "b"]);
        plain.toggle_section(W, 5);
        plain.toggle_all(W, 5);
        assert_eq!(texts(&plain.window(W, 5)), ["a", "b"]);
    }

    #[test]
    fn position_initiale_sur_la_premiere_etape_en_echec() {
        let v = sectioned(&[
            ("a", Status::Success, 9),
            ("b", Status::Failure, 2),
            ("c", Status::Failure, 1),
        ]);
        assert_eq!(v.initial_start(false), Some(Start::Line(10)));
        assert_eq!(
            v.row_of_line(W, 10),
            1,
            "le titre de « b » est la 2e ligne d'écran (« a » repliée)"
        );
        let ok = sectioned(&[("a", Status::Success, 1)]);
        assert_eq!(ok.initial_start(false), None);
        assert_eq!(ok.initial_start(true), Some(Start::End));
    }

    #[test]
    fn sections_et_retour_a_la_ligne_ensemble() {
        let mut v = sectioned(&[("a", Status::Failure, 1)]);
        v.wrap = true;
        v.set_log(&JobLog {
            steps: vec![StepLog {
                name: "a".into(),
                status: Status::Failure,
                lines: vec!["abcdefghi".into()],
            }],
            notice: None,
        });
        assert_eq!(texts(&v.window(3, 10)), ["▾ a", "abc", "def", "ghi"]);
        v.scroll = 2; // « def » : toujours la section « a »
        assert_eq!(v.current_step(3), Some(0));
        v.toggle_section(3, 10);
        assert_eq!(texts(&v.window(3, 10)), ["▸ a", " (1", " li", "gne", ")"]);
    }

    #[test]
    fn les_fleches_atteignent_chaque_section_meme_quand_tout_tient_a_l_ecran() {
        // Cas d'un job réussi : tout est replié et rien ne défile, il faut quand même pouvoir
        // ouvrir la 2e et la 3e étape.
        let mut v = sectioned(&[
            ("a", Status::Success, 5),
            ("b", Status::Success, 5),
            ("c", Status::Success, 5),
        ]);
        assert_eq!(v.total_rows(W), 3);
        assert_eq!(v.max_scroll(W, 20), 0, "rien à défiler");
        assert_eq!(v.current_step(W), Some(0));
        v.select_section(1, W, 20);
        assert_eq!(v.current_step(W), Some(1));
        let marked: Vec<_> = v
            .window(W, 20)
            .iter()
            .filter(|r| r.current)
            .map(|r| r.text)
            .collect();
        assert_eq!(
            marked,
            ["▸ b (5 lignes)"],
            "le titre sélectionné est marqué"
        );
        v.toggle_section(W, 20); // ouvre « b », pas « a »
        assert_eq!(
            texts(&v.window(W, 20)),
            [
                "▸ a (5 lignes)",
                "▾ b",
                "b 1",
                "b 2",
                "b 3",
                "b 4",
                "b 5",
                "▸ c (5 lignes)"
            ]
        );
        assert_eq!(v.current_step(W), Some(1), "la sélection reste sur « b »");
        v.select_section(1, W, 20);
        assert_eq!(v.current_step(W), Some(2));
        v.select_section(1, W, 20);
        assert_eq!(v.current_step(W), Some(2), "borné à la dernière");
        v.select_section(-1, W, 20);
        v.select_section(-1, W, 20);
        v.select_section(-1, W, 20);
        assert_eq!(v.current_step(W), Some(0), "borné à la première");
    }

    #[test]
    fn defiler_rend_la_selection_a_la_ligne_du_haut() {
        let mut v = sectioned(&[("a", Status::Failure, 5), ("b", Status::Failure, 5)]);
        v.select_section(1, W, 3);
        assert_eq!(v.current_step(W), Some(1));
        let max = v.max_scroll(W, 3);
        v.scroll_to_row(1, max); // l'utilisateur défile : on repart de la ligne du haut
        assert_eq!(v.current_step(W), Some(0));
    }

    #[test]
    fn changer_de_section_ne_fait_defiler_que_si_le_titre_est_hors_de_l_ecran() {
        let mut v = sectioned(&[
            ("a", Status::Failure, 2),
            ("b", Status::Failure, 20),
            ("c", Status::Failure, 2),
        ]);
        // Fenêtre de 6 lignes sur 28 : « b » (titre en 3e ligne) est visible, pas « c ».
        v.select_section(1, W, 6);
        assert_eq!(v.scroll, 0, "le titre était déjà visible : rien ne bouge");
        v.select_section(1, W, 6);
        // 27 lignes d'écran, fenêtre de 6 : le défilement maximal est 21 (le titre de « c » est en 24).
        assert_eq!(
            v.scroll, 21,
            "« c » est hors écran : on défile, borné au maximum"
        );
        v.select_section(-1, W, 6);
        assert_eq!(v.scroll, 3, "et « b » revient en haut quand on remonte");
    }

    #[test]
    fn selection_sans_section_ou_devenue_invalide() {
        let mut plain = view(&["a"]);
        plain.select_section(1, W, 5);
        assert_eq!(plain.current_step(W), None);
        let mut v = sectioned(&[("a", Status::Failure, 1), ("b", Status::Failure, 1)]);
        v.select_section(1, W, 10);
        // Un rafraîchissement qui perd une étape ne doit pas laisser une sélection hors limites.
        v.set_log(&joblog(&[("a", Status::Failure, 1)]));
        assert_eq!(v.current_step(W), Some(0));
        v.toggle_section(W, 10); // ne panique pas
    }

    #[test]
    fn longue_ligne_json_jusqu_a_la_derniere_ligne_d_ecran() {
        // Même contenu que la fixture réelle : un JSON de ~7000 caractères sur une seule ligne.
        let nums: Vec<String> = (0..1500).map(|i| i.to_string()).collect();
        let line = format!("{{\"data\": [{}]}}", nums.join(","));
        let mut v = view(&[&line]);
        v.wrap = true;
        let (w, h) = (98, 26);
        let total = v.total_rows(w);
        assert_eq!(
            total,
            crate::wrap::chunks(&line, w).len(),
            "l'index et le découpage s'accordent"
        );
        v.scroll = v.max_scroll(w, h);
        let win = v.window(w, h);
        assert_eq!(win.len(), h, "la dernière page est pleine");
        assert!(
            win.last().unwrap().text.ends_with("1499]}"),
            "la dernière ligne d'écran est atteinte : {:?}",
            win.last()
        );
    }

    // --- recherche

    fn search(v: &mut LogView, query: &str, w: usize, h: usize) {
        v.begin_search();
        v.prompt = Some(String::new());
        query.chars().for_each(|c| v.prompt_type(c));
        v.prompt_commit(w, h);
    }

    fn ranks(v: &LogView) -> (usize, usize) {
        v.search_status().map_or((9, 9), |(_, c, t)| (c, t))
    }

    #[test]
    fn la_recherche_compte_et_atteint_chaque_occurrence_en_boucle() {
        let lines: Vec<String> = (0..30)
            .map(|i| {
                if i % 10 == 3 {
                    format!("erreur {i}")
                } else {
                    format!("ligne {i}")
                }
            })
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut v = view(&refs);
        search(&mut v, "erreur", 40, 5);
        assert_eq!(ranks(&v), (1, 3), "première occurrence atteinte");
        assert_eq!(v.current_match().map(|m| m.line), Some(3));
        v.step_match(1, 40, 5);
        assert_eq!(v.current_match().map(|m| m.line), Some(13));
        assert_eq!(v.scroll, 13 - 5 / 3, "un peu de contexte au-dessus");
        v.step_match(1, 40, 5);
        v.step_match(1, 40, 5);
        assert_eq!(ranks(&v), (1, 3), "retour à la première en boucle");
        v.step_match(-1, 40, 5);
        assert_eq!(
            ranks(&v),
            (3, 3),
            "précédente depuis la première : la dernière"
        );
        // Le défilement ne dépasse jamais le bas
        assert!(v.scroll <= v.max_scroll(40, 5));
    }

    #[test]
    fn premiere_occurrence_a_partir_du_haut_de_l_ecran() {
        let mut v = view(&["foo", "bar", "foo", "bar", "foo"]);
        v.scroll = 3;
        search(&mut v, "foo", 40, 2);
        assert_eq!(
            v.current_match().map(|m| m.line),
            Some(4),
            "après le haut de l'écran"
        );
        // Précédente (N) depuis un nouvel état : avant le haut de l'écran
        v.clear_search();
        v.scroll = 3;
        search(&mut v, "foo", 40, 2);
        v.clear_search();
        v.begin_search();
        "foo".chars().for_each(|c| v.prompt_type(c));
        v.prompt_commit(40, 2);
        assert!(v.has_search());
    }

    #[test]
    fn casse_intelligente_et_requetes_sans_resultat() {
        let mut v = view(&["Erreur", "erreur", "ERREUR"]);
        search(&mut v, "erreur", 40, 5);
        assert_eq!(ranks(&v).1, 3);
        search(&mut v, "Erreur", 40, 5);
        assert_eq!(ranks(&v).1, 1, "une majuscule : la casse compte");
        search(&mut v, "absent", 40, 5);
        assert_eq!(v.search_status(), Some(("absent", 0, 0)));
        v.step_match(1, 40, 5); // sans effet, sans plantage
        assert_eq!(v.current_match(), None);
        // Saisie vide : efface la recherche
        v.begin_search();
        assert_eq!(v.prompt.as_deref(), Some("absent"), "préremplie");
        (0..6).for_each(|_| v.prompt_backspace());
        v.prompt_commit(40, 5);
        assert!(!v.has_search());
    }

    #[test]
    fn esc_efface_la_recherche_puis_ne_fait_plus_rien() {
        let mut v = view(&["abc"]);
        assert!(!v.clear_search());
        search(&mut v, "b", 40, 5);
        assert!(v.clear_search());
        assert!(!v.has_search() && v.current_match().is_none());
        // Annuler la saisie ne change pas une recherche existante
        search(&mut v, "a", 40, 5);
        v.begin_search();
        v.prompt_type('x');
        v.prompt_cancel();
        assert_eq!(v.search_status().map(|s| s.0), Some("a"));
    }

    #[test]
    fn une_occurrence_dans_une_etape_pliee_la_deplie() {
        let mut v = sectioned(&[
            ("Set up job", Status::Success, 5),
            ("Tests", Status::Failure, 3),
        ]);
        // « Set up job 4 » est dans l'étape réussie, repliée
        assert!(v.window(W, 20).iter().all(|r| r.text != "Set up job 4"));
        search(&mut v, "job 4", W, 20);
        assert_eq!(ranks(&v), (1, 1));
        assert!(
            v.window(W, 20).iter().any(|r| r.text == "Set up job 4"),
            "l'étape s'est dépliée pour montrer l'occurrence"
        );
        // Les titres d'étape ne sont pas du texte : « lignes » n'y est pas cherché
        search(&mut v, "▾", W, 20);
        assert_eq!(ranks(&v).1, 0);
        search(&mut v, "lignes)", W, 20);
        assert_eq!(
            ranks(&v).1,
            0,
            "le titre replié « (5 lignes) » est de l'habillage"
        );
    }

    #[test]
    fn occurrence_a_cheval_sur_deux_lignes_d_ecran_et_defilement_vers_la_bonne() {
        // Une ligne de 40 caractères repliée à 10 colonnes : l'occurrence est sur la 4e ligne d'écran
        let line = format!("{}CIBLE{}", "x".repeat(32), "y".repeat(3));
        let mut v = view(&["avant", &line, "après"]);
        v.wrap = true;
        let (w, h) = (10, 3);
        search(&mut v, "CIBLE", w, h);
        let m = v.current_match().unwrap();
        assert_eq!((m.line, m.start, m.end), (1, 32, 37));
        // Les lignes d'écran 1+3 = 4 (la ligne 1 débute à la ligne d'écran 1) ; contexte : h/3 = 1
        assert_eq!(v.scroll, v.row_of_line(w, 1) + 3 - 1);
        // La fenêtre montre bien un morceau de la ligne qui contient le début de l'occurrence
        let win = v.window(w, h);
        assert!(
            win.iter()
                .any(|r| r.line_no == 1 && r.offset <= 32 && 32 < r.offset + r.text.len())
        );
        // Le Row porte la ligne entière pour que l'affichage retrouve l'occurrence
        assert!(win.iter().all(|r| r.line == v.lines()[r.line_no].text));
    }

    #[test]
    fn la_recherche_survit_au_rafraichissement_et_au_flux_en_direct() {
        let mut v = sectioned(&[("Tests", Status::Running, 3)]);
        search(&mut v, "tests 2", W, 20);
        assert_eq!(ranks(&v), (1, 1));
        // Le log grandit : nouvelles occurrences, l'occurrence courante est gardée
        v.set_log(&joblog(&[("Tests", Status::Running, 8)]));
        assert_eq!(ranks(&v).1, 1);
        assert_eq!(v.current_match().map(|m| m.line), Some(2));
        v.set_log(&joblog(&[
            ("Tests", Status::Running, 8),
            ("Tests", Status::Failure, 2),
        ]));
        assert_eq!(ranks(&v).1, 2, "les nouvelles lignes sont comptées");
        // Texte remplacé (fichier rechargé) : recompté
        let mut f = view(&["un deux", "deux"]);
        search(&mut f, "deux", W, 5);
        f.set_plain(vec!["rien".into()]);
        assert_eq!(f.search_status(), Some(("deux", 0, 0)));
    }
}
