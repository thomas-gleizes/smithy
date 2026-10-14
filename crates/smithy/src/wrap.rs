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

/// Découpe `text` pour une largeur donnée, avec deux options pour le texte formaté (Markdown) :
/// `words` coupe de préférence après une espace (un mot plus long que la ligne est coupé au
/// caractère), et `hang` colonnes sont réservées au début de chaque ligne **suivante** (le
/// retrait d'une puce de liste, la barre d'une citation). Sans option, c'est `ranges`.
pub fn layout(text: &str, width: usize, hang: usize, words: bool) -> Vec<(usize, usize)> {
    if !words && hang == 0 {
        return ranges(text, width);
    }
    let width = width.max(1);
    let later = width.saturating_sub(hang).max(1);
    let mut out = Vec::new();
    let (mut start, mut used, mut limit) = (0, 0, width);
    // Dernière coupe possible de la ligne en cours : juste après une espace.
    let mut soft: Option<usize> = None;
    for (i, c) in text.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > limit && i > start {
            let cut = match soft {
                Some(b) if words && b > start => b,
                _ => i,
            };
            out.push((start, cut));
            start = cut;
            limit = later;
            used = text[cut..i].chars().map(|c| c.width().unwrap_or(0)).sum();
            soft = None;
        }
        used += w;
        if c == ' ' {
            soft = Some(i + 1);
        }
    }
    out.push((start, text.len()));
    out
}

/// Nombre de lignes d'écran de ce que ferait `layout`.
pub fn count(text: &str, width: usize, hang: usize, words: bool) -> usize {
    if !words && hang == 0 {
        rows(text, width)
    } else {
        layout(text, width, hang, words).len()
    }
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
    fn coupe_apres_une_espace_et_retrait_des_lignes_suivantes() {
        let text = "un deux trois quatre";
        let cut = |w, h, words| {
            layout(text, w, h, words)
                .into_iter()
                .map(|(a, b)| &text[a..b])
                .collect::<Vec<_>>()
        };
        // Par mots : la coupe suit l'espace, qui reste en fin de ligne
        assert_eq!(cut(9, 0, true), ["un deux ", "trois ", "quatre"]);
        // Au caractère (sans l'option) : comme `chunks`
        assert_eq!(cut(9, 0, false), chunks(text, 9));
        // Retrait de 2 colonnes : les lignes suivantes sont plus étroites
        assert_eq!(cut(10, 2, true), ["un deux ", "trois ", "quatre"]);
        // Un mot plus long que la ligne est coupé au caractère
        let long = "abcdefghij kl";
        let parts: Vec<_> = layout(long, 4, 0, true)
            .into_iter()
            .map(|(a, b)| &long[a..b])
            .collect();
        assert_eq!(parts, ["abcd", "efgh", "ij ", "kl"]);
        // Largeur nulle ou retrait plus grand que la largeur : pas de boucle infinie
        assert!(!layout(text, 0, 5, true).is_empty());
        assert!(!layout(text, 3, 9, true).is_empty());
    }

    #[test]
    fn le_nombre_de_lignes_suit_toujours_le_decoupage() {
        let samples = [
            "",
            "a",
            "un deux trois quatre cinq six sept",
            "mot-très-long-sans-espace-du-tout-vraiment",
            "日本語 の テキスト と des accents é à ü",
            "  espaces   multiples   ",
            "fin avec espace ",
        ];
        for text in samples {
            for width in [1, 2, 3, 5, 8, 13, 40] {
                for hang in [0, 1, 2, 4] {
                    for words in [false, true] {
                        assert_eq!(
                            count(text, width, hang, words),
                            layout(text, width, hang, words).len(),
                            "{text:?} w={width} h={hang} words={words}"
                        );
                        // Les morceaux se touchent et couvrent tout le texte
                        let parts = layout(text, width, hang, words);
                        assert_eq!(parts.first().map(|p| p.0), Some(0));
                        assert_eq!(parts.last().map(|p| p.1), Some(text.len()));
                        assert!(parts.windows(2).all(|w| w[0].1 == w[1].0));
                    }
                }
            }
        }
    }

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
