//! Graphe des commits (voies, fusions), calculé sans terminal sur une liste du plus récent au plus ancien.
//!
//! Chaque voie attend l'empreinte du prochain commit qu'elle doit relier. Une ligne du graphe occupe
//! deux colonnes par voie (glyphe + liaison horizontale) ; la couleur d'une cellule est l'indice de
//! sa voie, que l'affichage traduit en couleur ANSI.

use smithy_core::Commit;

/// Voies affichées au plus : au-delà, le graphe est tronqué à droite.
pub const MAX_LANES: usize = 8;

/// Une cellule du graphe : le caractère et l'indice de voie qui le colore.
pub type Cell = (char, usize);

/// Calcule une ligne de graphe par commit. Toutes les lignes ont la même largeur.
pub fn layout(commits: &[Commit]) -> Vec<Vec<Cell>> {
    let mut lanes: Vec<Option<&str>> = Vec::new();
    let mut rows: Vec<Vec<Cell>> = Vec::with_capacity(commits.len());
    for c in commits {
        let sha = c.sha.as_str();
        let matches: Vec<usize> = (0..lanes.len())
            .filter(|&i| lanes[i] == Some(sha))
            .collect();
        let col = match matches.first() {
            Some(&i) => i,
            None => free_slot(&lanes, &[]),
        };
        if col >= lanes.len() {
            lanes.resize(col + 1, None);
        }
        let width = lanes.len().max(col + 1);
        let mut row: Vec<Cell> = vec![(' ', 0); width * 2];
        for (i, lane) in lanes.iter().enumerate() {
            if lane.is_some() && !matches.contains(&i) {
                row[i * 2] = ('│', i);
            }
        }
        // Voies qui se rejoignent sur ce commit (venues du dessus, à droite de `col`).
        for &i in matches.iter().skip(1) {
            row[i * 2] = ('╯', i);
            join(&mut row, col, i, i);
        }
        row[col * 2] = ('●', col);
        for &i in &matches {
            lanes[i] = None;
        }
        // Premier parent : la voie continue ; les autres : une voie existante ou une nouvelle.
        let mut parents = c.parents.iter().map(String::as_str);
        lanes[col] = parents.next();
        for p in parents {
            if let Some(j) = lanes.iter().position(|l| *l == Some(p)) {
                let glyph = if j > col { '┤' } else { '├' };
                row[j * 2] = (glyph, j);
                join(&mut row, col, j, j);
            } else {
                let mut taken = matches.clone();
                taken.push(col);
                let k = free_slot(&lanes, &taken);
                if k >= lanes.len() {
                    lanes.resize(k + 1, None);
                    row.resize(lanes.len() * 2, (' ', 0));
                }
                lanes[k] = Some(p);
                row[k * 2] = (if k > col { '╮' } else { '╭' }, k);
                join(&mut row, col, k, k);
            }
        }
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
        rows.push(row);
    }
    let width = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .min(MAX_LANES * 2);
    for r in &mut rows {
        r.resize(width, (' ', 0));
    }
    rows
}

/// Largeur en colonnes du graphe de ces lignes.
pub fn width(rows: &[Vec<Cell>]) -> usize {
    rows.first().map_or(0, Vec::len)
}

/// Première voie libre qui n'est pas dans `taken`, sinon une nouvelle à droite.
fn free_slot(lanes: &[Option<&str>], taken: &[usize]) -> usize {
    (0..lanes.len())
        .find(|i| lanes[*i].is_none() && !taken.contains(i))
        .unwrap_or(
            lanes
                .len()
                .max(taken.iter().map(|t| t + 1).max().unwrap_or(0)),
        )
}

/// Trace la liaison horizontale entre deux voies (extrémités exclues) ; un `│` croisé devient `┼`.
fn join(row: &mut [Cell], a: usize, b: usize, color: usize) {
    let (lo, hi) = (a.min(b) * 2 + 1, a.max(b) * 2);
    for cell in &mut row[lo..hi] {
        *cell = match cell.0 {
            '│' => ('┼', cell.1),
            ' ' => ('─', color),
            _ => *cell,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sha: &str, parents: &[&str]) -> Commit {
        Commit {
            sha: sha.into(),
            message: String::new(),
            author: String::new(),
            date: None,
            url: None,
            parents: parents.iter().map(|p| (*p).into()).collect(),
        }
    }

    fn text(rows: &[Vec<Cell>]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|c| c.0)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn historique_lineaire() {
        let rows = layout(&[c("c", &["b"]), c("b", &["a"]), c("a", &[])]);
        assert_eq!(text(&rows), ["●", "●", "●"]);
    }

    #[test]
    fn branche_puis_fusion() {
        // m fusionne f (branche) dans main ; b est le point de départ commun.
        let rows = layout(&[
            c("m", &["x", "f"]),
            c("f", &["b"]),
            c("x", &["b"]),
            c("b", &[]),
        ]);
        assert_eq!(text(&rows), ["●─╮", "│ ●", "● │", "●─╯"]);
    }

    #[test]
    fn fusion_vers_une_voie_existante() {
        // Deux branches ouvertes en parallèle, puis une fusion qui rejoint une voie déjà présente.
        let rows = layout(&[
            c("m1", &["m2", "f"]),
            c("m2", &["a", "f"]),
            c("f", &["a"]),
            c("a", &[]),
        ]);
        let t = text(&rows);
        assert_eq!(t[0], "●─╮");
        assert_eq!(t[1], "●─┤");
        assert_eq!(t[2], "│ ●");
        assert_eq!(t[3], "●─╯");
    }

    #[test]
    fn largeur_identique_et_bornee() {
        // 12 sommets sans parent connu en amont : chaque tête ouvre sa voie.
        let commits: Vec<Commit> = (0..12)
            .map(|i| c(&format!("t{i}"), &[&format!("p{i}")]))
            .collect();
        let rows = layout(&commits);
        assert!(rows.iter().all(|r| r.len() == MAX_LANES * 2));
    }

    #[test]
    fn liste_vide() {
        assert!(layout(&[]).is_empty());
        assert_eq!(width(&[]), 0);
    }
}
