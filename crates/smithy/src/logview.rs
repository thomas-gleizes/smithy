//! Vue plein écran de la sortie d'un job : lignes, défilement et retour à la ligne.
//!
//! Le défilement se compte en **lignes d'écran** et non en lignes de log : une seule ligne
//! (un blob JSON de 5000 caractères) peut dépasser la hauteur du terminal une fois repliée,
//! et il faut pouvoir la lire jusqu'au bout. Sans retour à la ligne, les deux coïncident.
//! Aucune dépendance au terminal : la largeur et la hauteur utiles sont passées par l'appelant.

use std::cell::RefCell;

use smithy_core::{Pipeline, Repo, Status};

use crate::wrap;

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

/// Une ligne d'écran : un morceau de ligne de log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row<'a> {
    pub kind: LineKind,
    pub text: &'a str,
    /// Suite d'une ligne repliée (et non début d'une ligne de log).
    pub continuation: bool,
}

/// Début de chaque ligne de log, en lignes d'écran (`starts[i]`), plus le total à la fin.
/// Mis en cache : le recalcul coûte O(lignes), pas question de le refaire à chaque image.
#[derive(Debug)]
struct RowIndex {
    /// 0 quand le repli est désactivé : la largeur n'a alors aucun effet.
    width: usize,
    starts: Vec<usize>,
}

/// Vue plein écran de la sortie d'un job.
#[derive(Debug)]
pub struct LogView {
    pub repo: Repo,
    pub pipeline: Pipeline,
    pub state: LogState,
    /// Première ligne d'écran affichée.
    pub scroll: usize,
    /// Réserve du backend (ex. version du serveur non vérifiée).
    pub notice: Option<String>,
    /// Retour à la ligne actif.
    pub wrap: bool,
    index: RefCell<Option<RowIndex>>,
}

impl LogView {
    pub fn new(repo: Repo, pipeline: Pipeline) -> Self {
        Self {
            repo,
            pipeline,
            state: LogState::Loading,
            scroll: 0,
            notice: None,
            wrap: false,
            index: RefCell::new(None),
        }
    }

    /// Remplace le contenu ; le défilement est à recaler par l'appelant.
    pub fn set_lines(&mut self, lines: Vec<LogLine>) {
        self.state = LogState::Ready(lines);
        *self.index.borrow_mut() = None;
    }

    fn lines(&self) -> &[LogLine] {
        match &self.state {
            LogState::Ready(lines) => lines,
            _ => &[],
        }
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
                total += if self.wrap {
                    wrap::rows(&l.text, key)
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

    /// Ligne de log qui contient la ligne d'écran `row`.
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

    /// Bascule le retour à la ligne en gardant la même ligne de log en haut de l'écran.
    pub fn toggle_wrap(&mut self, width: usize, height: usize) {
        let top_line = self.line_of_row(width, self.scroll);
        self.wrap = !self.wrap;
        self.scroll = self
            .row_of_line(width, top_line)
            .min(self.max_scroll(width, height));
    }

    /// Les lignes d'écran visibles : de `scroll` sur `height` lignes.
    pub fn window(&self, width: usize, height: usize) -> Vec<Row<'_>> {
        if height == 0 {
            return Vec::new();
        }
        let lines = self.lines();
        let first_line = self.line_of_row(width, self.scroll);
        let first_row = self.row_of_line(width, first_line);
        let mut skip = self.scroll.saturating_sub(first_row);
        let mut out = Vec::with_capacity(height);
        for line in lines.iter().skip(first_line) {
            if self.wrap {
                for (n, chunk) in wrap::chunks(&line.text, width).into_iter().enumerate() {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    out.push(Row {
                        kind: line.kind,
                        text: chunk,
                        continuation: n > 0,
                    });
                    if out.len() == height {
                        return out;
                    }
                }
            } else {
                out.push(Row {
                    kind: line.kind,
                    text: &line.text,
                    continuation: false,
                });
                if out.len() == height {
                    return out;
                }
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
}
