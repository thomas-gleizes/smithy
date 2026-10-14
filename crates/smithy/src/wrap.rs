//! Retour à la ligne « dur », au caractère : un chemin ou un blob JSON se lit mieux coupé
//! à la largeur exacte que replié sur des mots qu'il n'a pas. Pur, donc testable seul.

use unicode_width::UnicodeWidthChar;

/// Découpe `text` en morceaux d'au plus `width` colonnes (un caractère large de 2 colonnes
/// compte pour 2). Toujours au moins un morceau, même pour une ligne vide ; une largeur
/// trop étroite pour un caractère le laisse seul sur sa ligne plutôt que de boucler.
#[cfg(test)]
pub fn chunks(text: &str, width: usize) -> Vec<&str> {
    ranges(text, width)
        .into_iter()
        .map(|(a, b)| &text[a..b])
        .collect()
}

/// Comme `chunks`, mais en positions d'octets `(début, fin)` : la coloration en a besoin pour
/// savoir quelle part de la ligne chaque morceau montre.
pub fn ranges(text: &str, width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let mut out = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (i, c) in text.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > width && i > start {
            out.push((start, i));
            start = i;
            used = 0;
        }
        used += w;
    }
    out.push((start, text.len()));
    out
}

/// Nombre de lignes d'écran de `text`, sans allouer.
pub fn rows(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let (mut rows, mut used, mut first) = (1, 0, true);
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width && !first {
            rows += 1;
            used = 0;
        }
        used += w;
        first = false;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coupe_a_la_largeur() {
        assert_eq!(chunks("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(chunks("abcdef", 3), ["abc", "def"]);
        assert_eq!(chunks("abc", 10), ["abc"]);
        assert_eq!(chunks("", 5), [""]);
    }

    #[test]
    fn caracteres_larges_et_accents() {
        // Chaque idéogramme occupe 2 colonnes : deux par ligne de 4.
        assert_eq!(chunks("日本語テスト", 4), ["日本", "語テ", "スト"]);
        assert_eq!(chunks("éàü", 2), ["éà", "ü"]);
        // Largeur 1 : un caractère large ne peut pas tenir, il reste seul (pas de boucle infinie).
        assert_eq!(chunks("日本", 1), ["日", "本"]);
        assert_eq!(chunks("a日b", 1), ["a", "日", "b"]);
    }

    #[test]
    fn largeur_nulle_traitee_comme_un() {
        assert_eq!(chunks("ab", 0), ["a", "b"]);
        assert_eq!(rows("ab", 0), 2);
    }

    #[test]
    fn rows_est_coherent_avec_chunks() {
        let cases = [
            "",
            "a",
            "abcdefghij",
            "日本語テスト",
            "a日b日c",
            "é\u{301}x",
            "🏁  Job succeeded",
        ];
        for text in cases {
            for width in 1..=12 {
                assert_eq!(
                    rows(text, width),
                    chunks(text, width).len(),
                    "{text:?} à {width}"
                );
            }
        }
    }

    #[test]
    fn les_morceaux_recomposent_le_texte() {
        let text = "ligne longue numéro 42 avec du texte 日本語 et des emoji 🏁🏁";
        for width in 1..=20 {
            assert_eq!(chunks(text, width).concat(), text, "largeur {width}");
        }
    }

    #[test]
    fn chaque_morceau_tient_dans_la_largeur() {
        use unicode_width::UnicodeWidthStr;
        for width in 2..=15 {
            for c in chunks("abc日本語def🏁xyz", width) {
                assert!(c.width() <= width, "{c:?} dépasse {width}");
            }
        }
    }
}
