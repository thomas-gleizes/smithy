//! Recherche de texte dans un volet (logs, fichier) : trouver les occurrences d'une requête dans
//! une ligne et les superposer aux couleurs de la coloration. Pur, donc testable seul.
//!
//! Casse « intelligente » comme dans les éditeurs : une requête sans majuscule ignore la casse,
//! une requête avec une majuscule la respecte.

use crate::highlight::Tok;

pub fn smart_case(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Les occurrences de `query` dans `text`, en positions d'octets `(début, fin)` sur des
/// frontières de caractères, sans chevauchement. Une requête vide ne trouve rien.
pub fn find_ranges(text: &str, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if query.is_empty() || text.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return text
            .match_indices(query)
            .map(|(i, m)| (i, i + m.len()))
            .collect();
    }
    // Sans casse : on cherche dans la version minuscule, en gardant pour chaque octet la position
    // du caractère d'origine (une minuscule peut avoir une autre longueur en octets).
    let needle = query.to_lowercase();
    let mut lowered = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len() + 1);
    for (i, c) in text.char_indices() {
        for l in c.to_lowercase() {
            let before = lowered.len();
            lowered.push(l);
            origin.extend(std::iter::repeat_n(i, lowered.len() - before));
        }
    }
    origin.push(text.len());
    lowered
        .match_indices(&needle)
        .filter_map(|(i, m)| {
            let (start, end) = (origin[i], origin[i + m.len()]);
            // Le dernier caractère trouvé peut avoir été étendu : on prend la fin de son caractère.
            let end = if end <= start {
                text[start..]
                    .chars()
                    .next()
                    .map_or(end, |c| start + c.len_utf8())
            } else {
                end
            };
            (start < end).then_some((start, end))
        })
        .collect()
}

/// Une occurrence est-elle la courante (celle de `n`/`N`) ou une autre ?
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    No,
    Other,
    Current,
}

/// Découpe les segments `(texte, jeton)` d'une ligne d'écran aux bornes des occurrences.
/// `offset` est la position du premier segment dans la ligne, `ranges` les occurrences de toute
/// la ligne, `current` celle à mettre en avant.
pub fn overlay<'a>(
    segments: Vec<(&'a str, Option<Tok>)>,
    offset: usize,
    ranges: &[(usize, usize)],
    current: Option<(usize, usize)>,
) -> Vec<(&'a str, Option<Tok>, Hit)> {
    let mut out = Vec::new();
    let mut pos = offset;
    for (text, tok) in segments {
        let (s, e) = (pos, pos + text.len());
        pos = e;
        if text.is_empty() {
            out.push((text, tok, Hit::No));
            continue;
        }
        let mut cuts = vec![s, e];
        for &(a, b) in ranges {
            cuts.extend([a, b].into_iter().filter(|&x| x > s && x < e));
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let hit = match ranges.iter().find(|&&(a, b)| a <= w[0] && w[1] <= b) {
                Some(&r) if Some(r) == current => Hit::Current,
                Some(_) => Hit::Other,
                None => Hit::No,
            };
            out.push((&text[w[0] - s..w[1] - s], tok, hit));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casse_intelligente() {
        assert!(!smart_case("erreur"));
        assert!(smart_case("Erreur"));
        // Minuscules : toutes les casses trouvées
        assert_eq!(
            find_ranges("Erreur erreur ERREUR", "erreur", false).len(),
            3
        );
        // Une majuscule dans la requête : casse respectée
        assert_eq!(
            find_ranges("Erreur erreur ERREUR", "Erreur", true),
            [(0, 6)]
        );
    }

    #[test]
    fn occurrences_sur_des_frontieres_de_caracteres() {
        let text = "été Été ÉTÉ — 日本語 日本";
        for query in ["été", "日本", "—", "é"] {
            for r in find_ranges(text, query, false) {
                assert!(
                    text.is_char_boundary(r.0) && text.is_char_boundary(r.1),
                    "{query}"
                );
                assert!(r.0 < r.1);
            }
        }
        assert_eq!(
            find_ranges(text, "été", false).len(),
            3,
            "accents et majuscules"
        );
        assert_eq!(
            find_ranges("日本語 日本", "日本", false),
            [(0, 6), (10, 16)]
        );
        // Pas de chevauchement, requête vide, texte vide
        assert_eq!(find_ranges("aaaa", "aa", true), [(0, 2), (2, 4)]);
        assert!(find_ranges("abc", "", false).is_empty());
        assert!(find_ranges("", "a", false).is_empty());
        assert!(find_ranges("abc", "d", false).is_empty());
    }

    #[test]
    fn caracteres_dont_la_minuscule_change_de_taille() {
        // « İ » (U+0130) devient « i̇ » (2 caractères) en minuscule : jamais de coupure invalide
        let text = "xİy İ";
        for r in find_ranges(text, "i", false) {
            assert!(
                text.is_char_boundary(r.0) && text.is_char_boundary(r.1),
                "{r:?}"
            );
        }
    }

    #[test]
    fn superposition_aux_couleurs() {
        // « let x = 1; » : mot-clé sur 0..3, recherche de « t x » (2..5) à cheval dessus
        let segs = vec![("let", Some(Tok::Keyword)), (" x = 1;", None)];
        let out = overlay(segs, 0, &[(2, 5)], None);
        let got: Vec<_> = out.iter().map(|(t, tok, h)| (*t, *tok, *h)).collect();
        assert_eq!(
            got,
            [
                ("le", Some(Tok::Keyword), Hit::No),
                ("t", Some(Tok::Keyword), Hit::Other),
                (" x", None, Hit::Other),
                (" = 1;", None, Hit::No),
            ]
        );
        // L'occurrence courante est distinguée
        let out = overlay(vec![("abab", None)], 0, &[(0, 2), (2, 4)], Some((2, 4)));
        let hits: Vec<_> = out.iter().map(|o| o.2).collect();
        assert_eq!(hits, [Hit::Other, Hit::Current]);
        // Ligne d'écran qui n'est pas le début de la ligne : décalage pris en compte
        let out = overlay(vec![("cd", None)], 2, &[(0, 3)], None);
        assert_eq!(
            out.iter().map(|o| (o.0, o.2)).collect::<Vec<_>>(),
            [("c", Hit::Other), ("d", Hit::No)]
        );
        // Ligne vide : un segment, sans occurrence
        assert_eq!(overlay(vec![("", None)], 0, &[], None).len(), 1);
    }
}
