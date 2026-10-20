//! Correctif unifié : analyse (fichiers, hunks, numéros de ligne), mots modifiés d'une ligne
//! remplacée et coloration syntaxique. Pur : aucune dépendance au terminal. Le texte d'une
//! ligne garde son préfixe (`+`, `-`, espace) : les décalages d'octets (régions, mots) le
//! comptent, ce qui laisse la recherche et le retour à la ligne travailler sur le texte affiché.

use smithy_core::{ChangeKind, clean_text_line};

use crate::highlight::{self, Run, Tok};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    Added,
    Removed,
    Context,
    /// En-tête `@@ -a,b +c,d @@`.
    Hunk,
    /// `---`, `+++`, mode, renommage, `\ No newline…`, `Binary files…`.
    Meta,
}

/// À quoi s'applique une coloration calculée à part.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DiffTarget {
    Commit(String),
    Pull(u64),
}

/// Ce que l'affichage doit savoir d'une ligne de correctif.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiffInfo {
    pub kind: DiffKind,
    /// Numéros de ligne avant et après (0 : la ligne n'existe pas de ce côté).
    pub old: u32,
    pub new: u32,
    /// Fichier du correctif (ordre des `diff --git`) et rang de la ligne dans ce fichier : la
    /// coloration calculée à part s'y retrouve.
    pub file: u32,
    pub idx: u32,
    /// Octets de `text` à mettre en évidence (mots modifiés d'une ligne remplacée).
    pub marks: Vec<(u32, u32)>,
}

pub struct DiffLine {
    pub kind: DiffKind,
    /// Texte nettoyé (`clean_text_line`), préfixe compris.
    pub text: String,
    pub old: u32,
    pub new: u32,
    pub marks: Vec<(u32, u32)>,
}

pub struct FileDiff {
    pub path: String,
    pub lines: Vec<DiffLine>,
    pub adds: usize,
    pub dels: usize,
    pub hint: Option<ChangeKind>,
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

/// Premières lignes de chaque côté d'un en-tête de hunk.
fn hunk_starts(header: &str) -> Option<(u32, u32)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.strip_prefix('+')?.split(' ').next()?;
    let first = |s: &str| s.split(',').next()?.parse().ok();
    Some((first(old)?, first(new)?))
}

/// Découpe un correctif en fichiers. Renvoie aussi si le plafond de lignes l'a coupé.
pub fn parse(patch: &str, limit: usize) -> (Vec<FileDiff>, bool) {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut total = 0usize;
    let mut cut = false;
    // État du hunk courant : numéros des prochaines lignes, `None` hors hunk.
    let mut at: Option<(u32, u32)> = None;
    let mut numbered = true;
    for raw in patch.lines() {
        if raw.starts_with("diff --git ") {
            files.push(FileDiff {
                path: file_of_header(raw),
                lines: Vec::new(),
                adds: 0,
                dels: 0,
                hint: None,
            });
            at = None;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        // L'empreinte des blobs n'apporte rien à la lecture.
        if at.is_none() && raw.starts_with("index ") {
            continue;
        }
        if total >= limit {
            cut = true;
            break;
        }
        total += 1;
        let (kind, old, new) = if raw.starts_with("@@") {
            let starts = hunk_starts(raw);
            numbered = starts.is_some();
            at = Some(starts.unwrap_or((0, 0)));
            (DiffKind::Hunk, 0, 0)
        } else if let Some((o, n)) = at.as_mut() {
            let mut take = |old: bool, new: bool| {
                let numbers = (
                    if old && numbered { *o } else { 0 },
                    if new && numbered { *n } else { 0 },
                );
                *o += u32::from(old);
                *n += u32::from(new);
                numbers
            };
            match raw.as_bytes().first() {
                Some(b'+') => {
                    file.adds += 1;
                    let (o, n) = take(false, true);
                    (DiffKind::Added, o, n)
                }
                Some(b'-') => {
                    file.dels += 1;
                    let (o, n) = take(true, false);
                    (DiffKind::Removed, o, n)
                }
                Some(b'\\') => (DiffKind::Meta, 0, 0),
                _ => {
                    let (o, n) = take(true, true);
                    (DiffKind::Context, o, n)
                }
            }
        } else {
            if raw.starts_with("new file mode") {
                file.hint = Some(ChangeKind::Added);
            } else if raw.starts_with("deleted file mode") {
                file.hint = Some(ChangeKind::Removed);
            } else if raw.starts_with("rename from") {
                file.hint = Some(ChangeKind::Renamed);
            }
            (DiffKind::Meta, 0, 0)
        };
        file.lines.push(DiffLine {
            kind,
            text: clean_text_line(raw),
            old,
            new,
            marks: Vec::new(),
        });
    }
    for file in &mut files {
        mark_words(&mut file.lines);
    }
    (files, cut)
}

// --- mots modifiés

/// Au-delà, on renonce (la table de comparaison est quadratique).
const MAX_TOKENS: usize = 300;
/// Part minimale de texte commun pour que deux lignes soient jugées « la même, retouchée » :
/// sinon tout serait surligné, ce qui n'apprend rien.
const MIN_COMMON: f32 = 0.3;

/// Les mots (lettres, chiffres, `_`), les blancs et chaque autre caractère, en octets.
fn tokens(s: &str) -> Vec<(usize, usize)> {
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut prev = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        match out.last_mut() {
            Some(last) if prev == Some(k) && k != 2 => last.1 = i + c.len_utf8(),
            _ => out.push((i, i + c.len_utf8())),
        }
        prev = Some(k);
    }
    out
}

/// Plages d'octets modifiées de chaque côté (`None` : trop différentes ou trop longues).
type Ranges = Vec<(usize, usize)>;

fn changed_words(old: &str, new: &str) -> Option<(Ranges, Ranges)> {
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > MAX_TOKENS || b.len() > MAX_TOKENS || a.is_empty() || b.is_empty() {
        return None;
    }
    let text = |s: &str, t: (usize, usize)| s[t.0..t.1].to_owned();
    let (ta, tb): (Vec<_>, Vec<_>) = (
        a.iter().map(|&t| text(old, t)).collect(),
        b.iter().map(|&t| text(new, t)).collect(),
    );
    // Plus longue sous-suite commune.
    let (n, m) = (ta.len(), tb.len());
    let mut lcs = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if ta[i] == tb[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if ta[i] == tb[j] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let common: usize = (0..n)
        .filter(|&i| keep_a[i])
        .map(|i| ta[i].trim().len())
        .sum();
    let longest = old.trim().len().max(new.trim().len());
    if longest == 0 || (common as f32) < MIN_COMMON * longest as f32 {
        return None;
    }
    Some((ranges(old, &a, &keep_a), ranges(new, &b, &keep_b)))
}

/// Les mots non conservés, regroupés (un blanc entre deux mots modifiés les relie).
fn ranges(s: &str, toks: &[(usize, usize)], keep: &[bool]) -> Ranges {
    let mut out: Ranges = Vec::new();
    for (&(start, end), &kept) in toks.iter().zip(keep) {
        if kept {
            continue;
        }
        match out.last_mut() {
            Some(last) if s[last.1..start].chars().all(char::is_whitespace) => last.1 = end,
            _ => out.push((start, end)),
        }
    }
    out
}

/// Pour chaque suite de lignes retirées suivie de lignes ajoutées, associe les lignes deux à
/// deux (la 1re retirée avec la 1re ajoutée…) et marque ce qui change entre elles.
fn mark_words(lines: &mut [DiffLine]) {
    let mut found: Vec<(usize, Vec<(u32, u32)>)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != DiffKind::Removed {
            i += 1;
            continue;
        }
        let removed = i;
        while i < lines.len() && lines[i].kind == DiffKind::Removed {
            i += 1;
        }
        let added = i;
        while i < lines.len() && lines[i].kind == DiffKind::Added {
            i += 1;
        }
        for k in 0..(added - removed).min(i - added) {
            let (o, n) = (&lines[removed + k].text, &lines[added + k].text);
            // Le préfixe (`-`, `+`) n'est pas du code : décalage de 1 octet.
            let Some((ro, rn)) = changed_words(&o[1..], &n[1..]) else {
                continue;
            };
            let shift = |r: Ranges| -> Vec<(u32, u32)> {
                r.into_iter()
                    .map(|(a, b)| (a as u32 + 1, b as u32 + 1))
                    .collect()
            };
            found.push((removed + k, shift(ro)));
            found.push((added + k, shift(rn)));
        }
    }
    for (i, marks) in found {
        lines[i].marks = marks;
    }
}

// --- coloration

fn prefix_run(kind: DiffKind) -> Option<Run> {
    let tok = match kind {
        DiffKind::Added => Tok::Added,
        DiffKind::Removed => Tok::Removed,
        _ => return None,
    };
    Some(Run {
        start: 0,
        end: 1,
        tok,
    })
}

/// Régions de chaque ligne du texte du fichier : le préfixe peint selon la nature de la ligne,
/// puis la coloration syntaxique du code. Chaque hunk est analysé seul, côté « après » (contexte
/// et lignes ajoutées) puis côté « avant » (contexte et lignes retirées) : on n'a pas le reste
/// du fichier, donc l'état de l'analyseur repart de zéro à chaque hunk.
pub fn highlight_file_diff(file: &FileDiff) -> Vec<Vec<Run>> {
    let mut out: Vec<Vec<Run>> = file
        .lines
        .iter()
        .map(|l| prefix_run(l.kind).into_iter().collect())
        .collect();
    let mut start = 0;
    while start < file.lines.len() {
        let end = file.lines[start + 1..]
            .iter()
            .position(|l| l.kind == DiffKind::Hunk)
            .map_or(file.lines.len(), |p| start + 1 + p);
        let hunk = &file.lines[start..end];
        for side in [DiffKind::Added, DiffKind::Removed] {
            let picked: Vec<usize> = (0..hunk.len())
                .filter(|&i| matches!(hunk[i].kind, DiffKind::Context) || hunk[i].kind == side)
                .collect();
            let code: Vec<String> = picked
                .iter()
                .map(|&i| {
                    let t = &hunk[i].text;
                    t[t.len().min(1)..].to_owned()
                })
                .collect();
            let Some(runs) = highlight::highlight_file(&file.path, &code) else {
                continue;
            };
            for (&i, runs) in picked.iter().zip(runs) {
                // Le contexte est pris côté « après » seulement ; l'autre côté ne sert qu'à
                // faire avancer l'analyseur.
                if hunk[i].kind == DiffKind::Context && side == DiffKind::Removed {
                    continue;
                }
                let shift = u32::from(!hunk[i].text.is_empty());
                out[start + i].extend(runs.into_iter().map(|r| Run {
                    start: r.start + shift,
                    end: r.end + shift,
                    tok: r.tok,
                }));
            }
        }
        start = end;
    }
    out
}

/// Coloration de tout un correctif : par fichier, par ligne (même découpage que `parse`).
pub fn highlight_patch(patch: &str, limit: usize) -> Vec<Vec<Vec<Run>>> {
    parse(patch, limit)
        .0
        .iter()
        .map(highlight_file_diff)
        .collect()
}

/// Empreinte d'un correctif : une coloration n'est appliquée qu'au texte pour lequel elle a été
/// calculée.
pub fn patch_key(patch: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    patch.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -10,3 +10,4 @@ fn x\n contexte\n-let vieux = 1;\n+let neuf = 1;\n+let autre = 2;\n fin\n";

    #[test]
    fn numeros_de_ligne_des_deux_cotes() {
        let (files, cut) = parse(PATCH, 100);
        assert!(!cut);
        let rows: Vec<_> = files[0]
            .lines
            .iter()
            .map(|l| (l.kind, l.old, l.new))
            .collect();
        assert_eq!(
            rows,
            [
                (DiffKind::Meta, 0, 0),
                (DiffKind::Meta, 0, 0),
                (DiffKind::Hunk, 0, 0),
                (DiffKind::Context, 10, 10),
                (DiffKind::Removed, 11, 0),
                (DiffKind::Added, 0, 11),
                (DiffKind::Added, 0, 12),
                (DiffKind::Context, 12, 13),
            ]
        );
        assert_eq!((files[0].adds, files[0].dels), (2, 1));
    }

    #[test]
    fn une_ligne_retiree_qui_ressemble_a_un_en_tete_reste_une_ligne_retiree() {
        let patch =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1 @@\n--- commentaire\n reste\n";
        let (files, _) = parse(patch, 100);
        let l = &files[0].lines[3];
        assert_eq!((l.kind, l.old), (DiffKind::Removed, 1));
        assert_eq!(files[0].dels, 1);
    }

    #[test]
    fn nouveau_fichier_sans_ancien_cote() {
        let patch = "diff --git a/n b/n\nnew file mode 100644\n--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+a\n+b\n\\ No newline at end of file\n";
        let (files, _) = parse(patch, 100);
        assert_eq!(files[0].hint, Some(ChangeKind::Added));
        let rows: Vec<_> = files[0].lines[3..]
            .iter()
            .map(|l| (l.kind, l.new))
            .collect();
        assert_eq!(
            rows,
            [
                (DiffKind::Hunk, 0),
                (DiffKind::Added, 1),
                (DiffKind::Added, 2),
                (DiffKind::Meta, 0)
            ]
        );
    }

    #[test]
    fn mots_modifies_d_une_ligne_remplacee() {
        let (files, _) = parse(PATCH, 100);
        let l = &files[0].lines;
        // « vieux » devient « neuf » : seul ce mot est marqué, préfixe compris dans les octets
        assert_eq!(
            &l[4].text[l[4].marks[0].0 as usize..l[4].marks[0].1 as usize],
            "vieux"
        );
        assert_eq!(
            &l[5].text[l[5].marks[0].0 as usize..l[5].marks[0].1 as usize],
            "neuf"
        );
        assert!(
            l[6].marks.is_empty(),
            "la 2e ligne ajoutée n'a pas de vis-à-vis"
        );
    }

    #[test]
    fn lignes_sans_rapport_pas_de_surlignage() {
        assert!(changed_words("abc def", "xyz uvw").is_none());
        assert!(changed_words("", "x").is_none());
    }

    #[test]
    fn mots_voisins_regroupes() {
        let (a, b) = changed_words("let a b = 1;", "let c d = 1;").unwrap();
        assert_eq!(a, [(4, 7)]);
        assert_eq!(b, [(4, 7)]);
    }

    #[test]
    fn coloration_par_cote_et_prefixe() {
        let (files, _) = parse(PATCH, 100);
        let styles = highlight_file_diff(&files[0]);
        // Préfixe peint, même avant toute coloration syntaxique
        assert_eq!(styles[4][0].tok, Tok::Removed);
        assert_eq!(styles[5][0].tok, Tok::Added);
        // `let` est un mot-clé Rust, décalé du préfixe
        let kw = |i: usize| {
            styles[i]
                .iter()
                .find(|r| r.tok == Tok::Keyword)
                .map(|r| (r.start, r.end))
        };
        assert_eq!(kw(4), Some((1, 4)));
        assert_eq!(kw(5), Some((1, 4)));
        assert_eq!(kw(3), None, "« contexte » n'est pas du code Rust");
    }

    #[test]
    fn langage_inconnu_garde_le_prefixe() {
        let patch = PATCH.replace("a.rs", "a.zzz");
        let styles = highlight_patch(&patch, 100);
        assert_eq!(styles[0][4].len(), 1);
        assert_eq!(styles[0][3].len(), 0);
    }

    #[test]
    fn plafond_de_lignes() {
        let (files, cut) = parse(PATCH, 6);
        assert!(cut);
        assert_eq!(files[0].lines.len(), 6);
    }
}
