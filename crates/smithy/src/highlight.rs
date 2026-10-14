//! Coloration syntaxique d'un fichier : les grammaires de syntect (jeu étendu de `two-face`)
//! analysent le texte, et chaque région est classée dans un petit nombre de jetons (`Tok`).
//! Le rendu choisit les couleurs : ce module ne connaît pas le terminal, et n'impose aucun
//! thème RGB (les jetons sont peints avec les 16 couleurs ANSI du terminal de l'utilisateur).
//!
//! Le calcul peut prendre du temps (chargement des grammaires, gros fichier) : il tourne hors du
//! fil de l'interface (`spawn_blocking` dans `main`) et son résultat arrive dans un second message.

use std::{path::Path, sync::OnceLock};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// Au-delà, le fichier est affiché sans couleur : le résultat serait trop long à attendre.
pub const MAX_LINES: usize = 20_000;
/// Une ligne plus longue (fichier minifié) n'est pas analysée.
pub const MAX_LINE_BYTES: usize = 1_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Comment,
    Str,
    Number,
    Constant,
    Keyword,
    Type,
    Function,
    Tag,
    Attr,
    Heading,
    Strong,
    Emph,
    Strike,
    Link,
    Quote,
    Code,
}

/// Une région colorée d'une ligne : octets `start..end` du texte affiché.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    pub start: u32,
    pub end: u32,
    pub tok: Tok,
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// Règles de classement, de la plus précise à la plus générale : la première qui préfixe un
/// scope gagne, et on regarde les scopes de l'intérieur vers l'extérieur.
fn rules() -> &'static [(Scope, Option<Tok>)] {
    static RULES: OnceLock<Vec<(Scope, Option<Tok>)>> = OnceLock::new();
    RULES.get_or_init(|| {
        const TABLE: &[(&str, Option<Tok>)] = &[
            ("comment", Some(Tok::Comment)),
            ("string", Some(Tok::Str)),
            ("constant.numeric", Some(Tok::Number)),
            ("constant", Some(Tok::Constant)),
            // Les opérateurs restent de la couleur du texte : sinon tout est coloré.
            ("keyword.operator", None),
            ("keyword", Some(Tok::Keyword)),
            ("storage", Some(Tok::Keyword)),
            ("entity.name.function", Some(Tok::Function)),
            ("support.function", Some(Tok::Function)),
            ("support.macro", Some(Tok::Function)),
            ("variable.function", Some(Tok::Function)),
            ("entity.name.tag", Some(Tok::Tag)),
            ("entity.other.attribute-name", Some(Tok::Attr)),
            ("entity.name", Some(Tok::Type)),
            ("entity.other.inherited-class", Some(Tok::Type)),
            ("support.type", Some(Tok::Type)),
            ("support.class", Some(Tok::Type)),
            ("markup.heading", Some(Tok::Heading)),
            ("markup.bold", Some(Tok::Strong)),
            ("markup.italic", Some(Tok::Emph)),
            ("markup.underline.link", Some(Tok::Link)),
            ("markup.quote", Some(Tok::Quote)),
            ("markup.raw", Some(Tok::Code)),
        ];
        TABLE
            .iter()
            .filter_map(|(s, t)| Scope::new(s).ok().map(|s| (s, *t)))
            .collect()
    })
}

fn classify(stack: &[Scope]) -> Option<Tok> {
    for scope in stack.iter().rev() {
        for (prefix, tok) in rules() {
            if prefix.is_prefix_of(*scope) {
                return *tok;
            }
        }
    }
    None
}

/// La grammaire d'un fichier : par nom (`Makefile`, `Dockerfile`), par extension, puis par la
/// première ligne (`#!/usr/bin/env python`). Rien n'est lu sur le disque.
fn syntax_for<'a>(path: &str, first_line: &str) -> Option<&'a SyntaxReference> {
    let set = syntaxes();
    let name = Path::new(path).file_name()?.to_str()?;
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);
    let found = set
        .find_syntax_by_extension(name)
        .or_else(|| ext.as_deref().and_then(|e| set.find_syntax_by_extension(e)))
        .or_else(|| set.find_syntax_by_first_line(first_line));
    found.filter(|s| s.name != "Plain Text")
}

/// Fusionne dans `runs` la région `start..end`, avec la région précédente si elle la touche.
fn push_run(runs: &mut Vec<Run>, start: usize, end: usize, tok: Tok) {
    if start >= end {
        return;
    }
    let (start, end) = (start as u32, end as u32);
    if let Some(last) = runs.last_mut()
        && last.tok == tok
        && last.end == start
    {
        last.end = end;
        return;
    }
    runs.push(Run { start, end, tok });
}

/// Découpe le texte d'une ligne d'écran en segments `(texte, jeton)` : `offset` est la position
/// du texte dans sa ligne, `runs` les régions de toute la ligne. Une région à cheval sur deux
/// lignes d'écran (retour à la ligne) est coupée avec elles.
pub fn segments<'a>(text: &'a str, offset: usize, runs: &[Run]) -> Vec<(&'a str, Option<Tok>)> {
    let end = offset + text.len();
    let mut out = Vec::new();
    let mut at = offset;
    for r in runs {
        let (s, e) = ((r.start as usize).max(at), (r.end as usize).min(end));
        if s >= e {
            continue;
        }
        if s > at {
            out.push((&text[at - offset..s - offset], None));
        }
        out.push((&text[s - offset..e - offset], Some(r.tok)));
        at = e;
    }
    if at < end {
        out.push((&text[at - offset..], None));
    }
    if out.is_empty() {
        out.push((text, None));
    }
    out
}

/// Colore un langage donné (nom de grammaire, ex. celui d'un bloc de code Markdown).
/// `None` si on ne le connaît pas.
pub fn highlight_as(lang: &str, lines: &[String]) -> Option<Vec<Vec<Run>>> {
    let set = syntaxes();
    let lang = lang.trim().to_lowercase();
    let syntax = set
        .find_syntax_by_token(&lang)
        .or_else(|| set.find_syntax_by_extension(&lang))
        .filter(|s| s.name != "Plain Text")?;
    Some(run(syntax, lines))
}

/// Colore les lignes d'un fichier (texte **déjà nettoyé**, tel qu'il est affiché : les
/// décalages d'octets des régions s'appliquent à ces lignes). `None` : langage inconnu ou
/// fichier trop long, il reste sans couleur.
pub fn highlight_file(path: &str, lines: &[String]) -> Option<Vec<Vec<Run>>> {
    if lines.len() > MAX_LINES {
        return None;
    }
    let syntax = syntax_for(path, lines.first().map_or("", String::as_str))?;
    Some(run(syntax, lines))
}

fn run(syntax: &SyntaxReference, lines: &[String]) -> Vec<Vec<Run>> {
    let set = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::with_capacity(lines.len());
    for text in lines {
        let mut runs = Vec::new();
        if text.len() > MAX_LINE_BYTES {
            out.push(runs);
            continue;
        }
        // Le jeu « newlines » attend la fin de ligne : sans elle, un commentaire `//` ne se
        // termine pas comme il faut.
        let line = format!("{text}\n");
        let Ok(ops) = state.parse_line(&line, set) else {
            out.push(runs);
            continue;
        };
        let mut prev = 0;
        for (at, op) in ops {
            if at > prev
                && let Some(tok) = classify(stack.as_slice())
            {
                push_run(&mut runs, prev, at.min(text.len()), tok);
            }
            if stack.apply(&op).is_err() {
                break;
            }
            prev = at;
        }
        if prev < text.len()
            && let Some(tok) = classify(stack.as_slice())
        {
            push_run(&mut runs, prev, text.len(), tok);
        }
        out.push(runs);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str) -> Vec<String> {
        src.lines().map(str::to_owned).collect()
    }

    /// Le jeton du premier caractère de `needle` dans la ligne `line`.
    fn tok_at(src: &[String], runs: &[Vec<Run>], line: usize, needle: &str) -> Option<Tok> {
        let at = src[line].find(needle)? as u32;
        runs[line]
            .iter()
            .find(|r| r.start <= at && at < r.end)
            .map(|r| r.tok)
    }

    #[test]
    fn rust_commentaire_chaine_mot_cle_nombre() {
        let src = lines("// un commentaire\nfn main() {\n    let s = \"salut\"; let n = 42;\n}");
        let runs = highlight_file("src/main.rs", &src).unwrap();
        assert_eq!(runs.len(), src.len());
        assert_eq!(tok_at(&src, &runs, 0, "un"), Some(Tok::Comment));
        // Le commentaire `//` s'arrête à la fin de sa ligne : le code suivant est analysé
        assert_eq!(tok_at(&src, &runs, 1, "fn"), Some(Tok::Keyword));
        assert_eq!(tok_at(&src, &runs, 1, "main"), Some(Tok::Function));
        assert_eq!(tok_at(&src, &runs, 2, "let"), Some(Tok::Keyword));
        assert_eq!(tok_at(&src, &runs, 2, "salut"), Some(Tok::Str));
        assert_eq!(tok_at(&src, &runs, 2, "42"), Some(Tok::Number));
        // Les espaces et l'opérateur `=` restent sans couleur
        assert_eq!(tok_at(&src, &runs, 2, "= "), None);
    }

    #[test]
    fn les_regions_collent_au_texte_affiche() {
        // Accents et caractères larges : les décalages sont des octets du texte affiché
        let src = lines("let é = \"日本\"; // fin");
        let runs = highlight_file("a.rs", &src).unwrap();
        for r in &runs[0] {
            assert!(src[0].is_char_boundary(r.start as usize), "{r:?}");
            assert!(src[0].is_char_boundary(r.end as usize), "{r:?}");
            assert!(r.end as usize <= src[0].len());
            assert!(r.start < r.end);
        }
        assert_eq!(tok_at(&src, &runs, 0, "日"), Some(Tok::Str));
        assert_eq!(tok_at(&src, &runs, 0, "fin"), Some(Tok::Comment));
    }

    #[test]
    fn langages_et_detection_sans_disque() {
        let one = |path: &str, text: &str| highlight_file(path, &lines(text));
        assert!(
            one("Cargo.toml", "[package]\nname = \"x\"").is_some(),
            "toml"
        );
        assert!(
            one("app.ts", "const a: number = 1;").is_some(),
            "typescript"
        );
        assert!(one("data.json", "{\"a\": 1}").is_some(), "json");
        assert!(
            one("Makefile", "all:\n\techo hi").is_some(),
            "nom de fichier"
        );
        assert!(one("Dockerfile", "FROM rust:1").is_some(), "Dockerfile");
        // Shebang sans extension
        assert!(
            one("script", "#!/usr/bin/env python\nprint(1)").is_some(),
            "shebang"
        );
        // Inconnu : pas de couleur
        assert!(one("notes.txt", "du texte").is_none());
        assert!(one("fichier.inconnu_xyz", "rien").is_none());
    }

    #[test]
    fn segments_d_une_ligne_repliee() {
        let run = |start, end, tok| Run { start, end, tok };
        let text = "ab cdefgh ij";
        let runs = [run(3, 9, Tok::Str), run(10, 12, Tok::Number)];
        // Ligne entière
        assert_eq!(
            segments(text, 0, &runs),
            [
                ("ab ", None),
                ("cdefgh", Some(Tok::Str)),
                (" ", None),
                ("ij", Some(Tok::Number))
            ]
        );
        // Repliée à 6 colonnes : la région « cdefgh » est coupée entre deux lignes d'écran
        let wrapped = crate::wrap::ranges(text, 6);
        assert_eq!(wrapped, [(0, 6), (6, 12)]);
        assert_eq!(
            segments(&text[0..6], 0, &runs),
            [("ab ", None), ("cde", Some(Tok::Str))]
        );
        assert_eq!(
            segments(&text[6..12], 6, &runs),
            [
                ("fgh", Some(Tok::Str)),
                (" ", None),
                ("ij", Some(Tok::Number))
            ]
        );
        // Sans couleur, ou ligne vide : un seul segment
        assert_eq!(segments("abc", 0, &[]), [("abc", None)]);
        assert_eq!(segments("", 0, &[]), [("", None)]);
        // Une région qui commence avant la ligne d'écran
        assert_eq!(
            segments("xyz", 3, &[run(0, 5, Tok::Comment)]),
            [("xy", Some(Tok::Comment)), ("z", None)]
        );
    }

    #[test]
    fn bloc_de_code_par_nom_de_langage() {
        let src = lines("fn x() {}");
        assert!(highlight_as("rust", &src).is_some());
        assert!(highlight_as("rs", &src).is_some());
        assert!(highlight_as("langage-inconnu", &src).is_none());
    }

    #[test]
    fn garde_fous() {
        // Ligne trop longue : sans couleur, mais le reste du fichier est analysé
        let long = format!("let a = \"{}\";", "x".repeat(MAX_LINE_BYTES + 10));
        let src = vec![long, "let b = 1;".to_owned()];
        let runs = highlight_file("a.rs", &src).unwrap();
        assert!(runs[0].is_empty() && !runs[1].is_empty());
        // Trop de lignes : rien
        let many = vec!["x".to_owned(); MAX_LINES + 1];
        assert!(highlight_file("a.rs", &many).is_none());
        // Lignes vides, fichier vide
        assert_eq!(highlight_file("a.rs", &[]), Some(vec![]));
        assert_eq!(highlight_file("a.rs", &lines("\n\n")).unwrap().len(), 2);
    }
}
