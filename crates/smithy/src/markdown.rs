//! Rendu d'un fichier Markdown en lignes de texte colorées pour le volet de texte.
//!
//! Pas de mise en page à la largeur : chaque paragraphe est **une** ligne logique, que le volet
//! replie par mots (`LogLine::words`) avec le retrait de ses puces (`LogLine::hang`). Les blocs
//! de code (replis au caractère) sont colorés par la grammaire de leur langage, les tableaux
//! sont alignés. Le texte produit ne dépend pas de la coloration : celle-ci n'ajoute que des
//! régions, calculées à part (voir `render` et son paramètre `highlight_code`).

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use smithy_core::clean_text_line;
use unicode_width::UnicodeWidthStr;

use crate::{
    highlight::{self, Run, Tok},
    logview::{LineKind, LogLine},
};

/// Une ligne rendue : le texte, ses régions colorées (octets de `text`), le début des lignes
/// d'écran suivantes si elle est repliée, et si elle l'est par mots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdLine {
    pub text: String,
    pub runs: Vec<Run>,
    pub hang: String,
    pub words: bool,
}

impl MdLine {
    pub fn into_log_line(self) -> LogLine {
        LogLine {
            kind: LineKind::Text,
            text: self.text,
            step: None,
            runs: self.runs,
            hang: self.hang,
            words: self.words,
            diff: None,
        }
    }
}

/// Le fichier est-il du Markdown ? (d'après son nom)
pub fn is_markdown(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "md" | "markdown" | "mdown" | "mkd"
        )
    })
}

const BULLET: &str = "• ";
const QUOTE_BAR: &str = "▎ ";
const CODE_BAR: &str = "│ ";
const RULE_LEN: usize = 40;

struct ListCtx {
    /// Prochain numéro d'une liste ordonnée.
    next: Option<u64>,
    /// Largeur de la puce de l'élément en cours.
    marker_width: usize,
}

struct TableB {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    header_rows: usize,
}

struct Builder {
    out: Vec<MdLine>,
    text: String,
    runs: Vec<Run>,
    styles: Vec<Tok>,
    quote: usize,
    lists: Vec<ListCtx>,
    marker: Option<String>,
    links: Vec<(String, usize)>,
    image: Option<String>,
    code: Option<(String, String)>,
    table: Option<TableB>,
    html: bool,
    highlight_code: bool,
}

impl Builder {
    fn new(highlight_code: bool) -> Self {
        Self {
            out: Vec::new(),
            text: String::new(),
            runs: Vec::new(),
            styles: Vec::new(),
            quote: 0,
            lists: Vec::new(),
            marker: None,
            links: Vec::new(),
            image: None,
            code: None,
            table: None,
            html: false,
            highlight_code,
        }
    }

    /// Ajoute du texte à la ligne en cours, dans le style courant (ou `tok`).
    fn push(&mut self, text: &str, tok: Option<Tok>) {
        let text = clean_text_line(text);
        if text.is_empty() {
            return;
        }
        let tok = tok.or_else(|| self.styles.last().copied());
        let start = self.text.len();
        self.text.push_str(&text);
        if let Some(tok) = tok {
            push_run(&mut self.runs, start, self.text.len(), tok);
        }
    }

    fn quote_prefix(&self) -> String {
        QUOTE_BAR.repeat(self.quote)
    }

    /// Début de la première ligne d'un bloc (barres de citation, retrait, puce) et des suivantes.
    fn prefixes(&mut self) -> (String, String) {
        let quote = self.quote_prefix();
        let (mut first, mut hang) = (quote.clone(), quote);
        let last = self.lists.len().saturating_sub(1);
        for (i, ctx) in self.lists.iter().enumerate() {
            let blank = " ".repeat(ctx.marker_width);
            hang.push_str(&blank);
            match (i == last, &self.marker) {
                (true, Some(m)) => first.push_str(m),
                _ => first.push_str(&blank),
            }
        }
        self.marker = None;
        (first, hang)
    }

    fn flush(&mut self) {
        if self.text.is_empty() && self.marker.is_none() {
            return;
        }
        let (first, hang) = self.prefixes();
        let shift = first.len();
        let mut runs = Vec::new();
        mark_quote(&first, &mut runs);
        runs.extend(self.runs.drain(..).map(|r| Run {
            start: r.start + shift as u32,
            end: r.end + shift as u32,
            tok: r.tok,
        }));
        self.out.push(MdLine {
            text: format!("{first}{}", std::mem::take(&mut self.text)),
            runs,
            hang,
            words: true,
        });
    }

    fn blank(&mut self) {
        if self.out.last().is_some_and(|l| !l.text.is_empty()) {
            self.out.push(MdLine {
                text: String::new(),
                runs: Vec::new(),
                hang: String::new(),
                words: false,
            });
        }
    }

    /// Une ligne qui ne se replie pas par mots (code, tableau, règle).
    fn literal(&mut self, prefix: &str, pieces: Vec<(String, Option<Tok>)>) {
        let mut text = prefix.to_owned();
        let mut runs = Vec::new();
        mark_quote(prefix, &mut runs);
        for (piece, tok) in pieces {
            let start = text.len();
            text.push_str(&piece);
            if let Some(tok) = tok {
                push_run(&mut runs, start, text.len(), tok);
            }
        }
        self.out.push(MdLine {
            text,
            runs,
            hang: String::new(),
            words: false,
        });
    }

    fn end_code(&mut self) {
        let Some((lang, text)) = self.code.take() else {
            return;
        };
        let lines: Vec<String> = text
            .trim_end_matches('\n')
            .split('\n')
            .map(clean_text_line)
            .collect();
        let styled = if self.highlight_code && !lang.is_empty() {
            highlight::highlight_as(&lang, &lines)
        } else {
            None
        };
        let prefix = self.quote_prefix();
        for (i, line) in lines.iter().enumerate() {
            let runs = styled.as_ref().map(|s| s[i].as_slice()).unwrap_or(&[]);
            let mut text = format!("{prefix}{CODE_BAR}");
            let mut out_runs = Vec::new();
            mark_quote(&prefix, &mut out_runs);
            push_run(&mut out_runs, prefix.len(), text.len(), Tok::Comment);
            let shift = text.len() as u32;
            text.push_str(line);
            out_runs.extend(runs.iter().map(|r| Run {
                start: r.start + shift,
                end: r.end + shift,
                tok: r.tok,
            }));
            // Sans grammaire, le code reste d'une couleur unie reconnaissable.
            if styled.is_none() && !line.is_empty() {
                push_run(&mut out_runs, shift as usize, text.len(), Tok::Code);
            }
            self.out.push(MdLine {
                text,
                runs: out_runs,
                hang: String::new(),
                words: false,
            });
        }
        self.blank();
    }

    fn end_table(&mut self) {
        let Some(t) = self.table.take() else { return };
        let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let width: Vec<usize> = (0..cols)
            .map(|c| {
                t.rows
                    .iter()
                    .filter_map(|r| r.get(c))
                    .map(|s| s.width())
                    .max()
                    .unwrap_or(0)
                    .max(1)
            })
            .collect();
        let prefix = self.quote_prefix();
        let rule = |l: &str, m: &str, r: &str| {
            let inner: Vec<String> = width.iter().map(|w| "─".repeat(w + 2)).collect();
            vec![(format!("{l}{}{r}", inner.join(m)), Some(Tok::Comment))]
        };
        self.literal(&prefix, rule("┌", "┬", "┐"));
        for (n, row) in t.rows.iter().enumerate() {
            let mut pieces = vec![("│".to_owned(), Some(Tok::Comment))];
            for (c, w) in width.iter().enumerate() {
                let cell = row.get(c).map_or("", String::as_str);
                let pad = w - cell.width();
                let align = t.aligns.get(c).copied().unwrap_or(Alignment::None);
                let (left, right) = match align {
                    Alignment::Right => (pad, 0),
                    Alignment::Center => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };
                let tok = (n < t.header_rows).then_some(Tok::Strong);
                pieces.push((
                    format!(" {}{cell}{} ", " ".repeat(left), " ".repeat(right)),
                    tok,
                ));
                pieces.push(("│".to_owned(), Some(Tok::Comment)));
            }
            self.literal(&prefix, pieces);
            if n + 1 == t.header_rows && n + 1 < t.rows.len() {
                self.literal(&prefix, rule("├", "┼", "┤"));
            }
        }
        self.literal(&prefix, rule("└", "┴", "┘"));
        self.blank();
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush();
                self.styles.push(Tok::Heading);
                let hashes = "#".repeat(level as usize);
                self.push(&format!("{hashes} "), None);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        l.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::HtmlBlock => {
                self.flush();
                self.html = true;
            }
            Tag::List(first) => {
                self.flush();
                self.lists.push(ListCtx {
                    next: first,
                    marker_width: 2,
                });
            }
            Tag::Item => {
                self.flush();
                if let Some(ctx) = self.lists.last_mut() {
                    let marker = match ctx.next.as_mut() {
                        Some(n) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        None => BULLET.to_owned(),
                    };
                    ctx.marker_width = marker.width();
                    self.marker = Some(marker);
                }
            }
            Tag::Table(aligns) => {
                self.flush();
                self.table = Some(TableB {
                    aligns,
                    rows: Vec::new(),
                    row: Vec::new(),
                    cell: String::new(),
                    header_rows: 0,
                });
            }
            Tag::Emphasis => self.styles.push(Tok::Emph),
            Tag::Strong => self.styles.push(Tok::Strong),
            Tag::Strikethrough => self.styles.push(Tok::Strike),
            Tag::Link { dest_url, .. } => {
                self.links.push((dest_url.to_string(), self.text.len()));
                self.styles.push(Tok::Link);
            }
            Tag::Image { dest_url, .. } => {
                self.links.push((dest_url.to_string(), self.text.len()));
                self.image = Some(String::new());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.flush();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
                if self.quote == 0 {
                    self.blank();
                }
            }
            TagEnd::CodeBlock => self.end_code(),
            TagEnd::HtmlBlock => {
                self.html = false;
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Table => self.end_table(),
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(std::mem::take(&mut t.row));
                    t.header_rows = t.rows.len();
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(std::mem::take(&mut t.row));
                }
            }
            TagEnd::TableCell => {
                if let Some(t) = self.table.as_mut() {
                    let cell = std::mem::take(&mut t.cell);
                    t.row.push(cell);
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some((dest, start)) = self.links.pop() {
                    let label = self.text[start..].to_owned();
                    // La cible s'affiche, comme sur un terminal sans souris : sauf si c'est le texte.
                    if !dest.is_empty() && label != dest {
                        self.push(&format!(" ({dest})"), Some(Tok::Comment));
                    }
                }
            }
            TagEnd::Image => {
                let alt = self.image.take().unwrap_or_default();
                let dest = self.links.pop().map(|l| l.0).unwrap_or_default();
                let alt = if alt.is_empty() {
                    "image".to_owned()
                } else {
                    alt
                };
                self.push(&format!("[image : {alt}]"), Some(Tok::Link));
                if !dest.is_empty() {
                    self.push(&format!(" ({dest})"), Some(Tok::Comment));
                }
            }
            _ => {}
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if let Some((_, code)) = self.code.as_mut() {
                    code.push_str(&t);
                } else if let Some(alt) = self.image.as_mut() {
                    alt.push_str(&t);
                } else if let Some(table) = self.table.as_mut() {
                    table.cell.push_str(&clean_text_line(&t));
                } else if self.html {
                    self.html_line(&t);
                } else {
                    self.push(&t, None);
                }
            }
            Event::Code(t) => {
                if let Some(alt) = self.image.as_mut() {
                    alt.push_str(&t);
                } else if let Some(table) = self.table.as_mut() {
                    table.cell.push_str(&clean_text_line(&t));
                } else {
                    self.push(&t, Some(Tok::Code));
                }
            }
            Event::Html(t) => self.html_line(&t),
            Event::InlineHtml(t) => {
                if self.table.is_none() {
                    self.push(&t, Some(Tok::Comment));
                }
            }
            Event::SoftBreak => {
                if self.image.is_some() {
                } else if let Some(table) = self.table.as_mut() {
                    table.cell.push(' ');
                } else {
                    self.push(" ", None);
                }
            }
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                let prefix = self.quote_prefix();
                self.literal(&prefix, vec![("─".repeat(RULE_LEN), Some(Tok::Comment))]);
                self.blank();
            }
            Event::TaskListMarker(done) => {
                self.marker = Some(if done { "☑ " } else { "☐ " }.to_owned());
            }
            _ => {}
        }
    }

    /// HTML brut : montré tel quel, estompé, une ligne par ligne.
    fn html_line(&mut self, raw: &str) {
        self.flush();
        let prefix = self.quote_prefix();
        for line in raw.trim_end_matches('\n').split('\n') {
            self.literal(&prefix, vec![(clean_text_line(line), Some(Tok::Comment))]);
        }
    }
}

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

/// Colore les barres de citation au début d'une ligne.
fn mark_quote(prefix: &str, runs: &mut Vec<Run>) {
    let mut at = 0;
    while prefix[at..].starts_with(QUOTE_BAR) {
        push_run(runs, at, at + QUOTE_BAR.len(), Tok::Quote);
        at += QUOTE_BAR.len();
    }
}

/// Rend un document. `highlight_code` colore les blocs de code par leur langage ; le texte
/// produit est le même avec ou sans (seules les régions changent), si bien qu'on peut rendre une
/// première fois sans coloration, tout de suite, puis appliquer la coloration quand elle arrive.
pub fn render(src: &str, highlight_code: bool) -> Vec<MdLine> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut b = Builder::new(highlight_code);
    for ev in Parser::new_ext(src, options) {
        b.event(ev);
    }
    b.flush();
    while b.out.last().is_some_and(|l| l.text.is_empty()) {
        b.out.pop();
    }
    if b.out.is_empty() {
        b.out.push(MdLine {
            text: "(document vide)".to_owned(),
            runs: Vec::new(),
            hang: String::new(),
            words: false,
        });
    }
    b.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<String> {
        render(src, false).into_iter().map(|l| l.text).collect()
    }

    /// Le jeton du texte `needle` dans la ligne `line`.
    fn tok_of(lines: &[MdLine], line: usize, needle: &str) -> Option<Tok> {
        let at = lines[line].text.find(needle)? as u32;
        lines[line]
            .runs
            .iter()
            .find(|r| r.start <= at && at < r.end)
            .map(|r| r.tok)
    }

    #[test]
    fn reconnait_les_fichiers_markdown() {
        for p in ["README.md", "docs/a.MD", "x.markdown", "n.mkd"] {
            assert!(is_markdown(p), "{p}");
        }
        for p in ["a.rs", "md", "notes.txt", "a.mdx", ""] {
            assert!(!is_markdown(p), "{p}");
        }
    }

    #[test]
    fn titres_paragraphes_et_style_en_ligne() {
        let lines = render(
            "# Titre\n\nUn **gras**, de l'*italique*, du `code` et ~~barré~~.\n",
            false,
        );
        let t: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            t,
            ["# Titre", "", "Un gras, de l'italique, du code et barré."]
        );
        assert_eq!(tok_of(&lines, 0, "Titre"), Some(Tok::Heading));
        assert_eq!(tok_of(&lines, 2, "gras"), Some(Tok::Strong));
        assert_eq!(tok_of(&lines, 2, "italique"), Some(Tok::Emph));
        assert_eq!(tok_of(&lines, 2, "code"), Some(Tok::Code));
        assert_eq!(tok_of(&lines, 2, "barré"), Some(Tok::Strike));
        assert_eq!(tok_of(&lines, 2, "Un"), None);
        // Un paragraphe est une seule ligne logique, repliée par mots
        assert!(lines[2].words && lines[2].hang.is_empty());
    }

    #[test]
    fn listes_imbriquees_avec_retrait() {
        let lines = render("- un\n- deux\n  - sous\n\n1. a\n2. b\n", false);
        let t: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(t, ["• un", "• deux", "  • sous", "", "1. a", "2. b"]);
        // Les lignes d'écran suivantes sont retirées de la largeur de la puce
        assert_eq!(lines[0].hang, "  ");
        assert_eq!(lines[2].hang, "    ");
        assert_eq!(lines[4].hang, "   ");
    }

    #[test]
    fn taches_et_citations() {
        let t = texts("- [x] fait\n- [ ] à faire\n\n> citation\n> suite\n");
        assert_eq!(t, ["☑ fait", "☐ à faire", "", "▎ citation suite"]);
        let lines = render("> a\n>\n> > b\n", false);
        assert_eq!(lines[0].text, "▎ a");
        assert_eq!(lines.last().unwrap().text, "▎ ▎ b");
        assert_eq!(lines.last().unwrap().hang, "▎ ▎ ");
        assert_eq!(tok_of(&lines, 0, "▎"), Some(Tok::Quote));
    }

    #[test]
    fn bloc_de_code_colore_par_langage() {
        let src = "```rust\nfn main() {}\n```\n\n```\nbrut\n```\n";
        let plain = render(src, false);
        let colored = render(src, true);
        // Le texte est le même avec ou sans coloration
        let texts = |v: &[MdLine]| v.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&plain), texts(&colored));
        assert_eq!(colored[0].text, "│ fn main() {}");
        assert!(!colored[0].words, "le code ne se replie pas par mots");
        assert_eq!(tok_of(&colored, 0, "fn"), Some(Tok::Keyword));
        assert_eq!(tok_of(&colored, 0, "│"), Some(Tok::Comment));
        // Sans coloration, le code a une couleur unie ; le bloc sans langage aussi
        assert_eq!(tok_of(&plain, 0, "fn"), Some(Tok::Code));
        assert_eq!(colored[2].text, "│ brut");
        assert_eq!(tok_of(&colored, 2, "brut"), Some(Tok::Code));
    }

    #[test]
    fn tableau_aligne() {
        let lines = render(
            "| Nom | Valeur |\n|-----|-------:|\n| a | 1 |\n| bb | 22 |\n",
            false,
        );
        let t: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            t,
            [
                "┌─────┬────────┐",
                "│ Nom │ Valeur │",
                "├─────┼────────┤",
                "│ a   │      1 │",
                "│ bb  │     22 │",
                "└─────┴────────┘",
            ]
        );
        assert_eq!(tok_of(&lines, 1, "Nom"), Some(Tok::Strong));
        assert!(lines.iter().all(|l| !l.words));
    }

    #[test]
    fn liens_images_html_et_regle() {
        let lines = render(
            "Voir [la page](https://ex.com/p) ou <https://ex.com>.\n\n![logo](l.png)\n\n---\n\n<div>brut</div>\n",
            false,
        );
        let t: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(t[0], "Voir la page (https://ex.com/p) ou https://ex.com.");
        assert_eq!(tok_of(&lines, 0, "la page"), Some(Tok::Link));
        assert_eq!(tok_of(&lines, 0, "(https://ex.com/p)"), Some(Tok::Comment));
        assert_eq!(t[2], "[image : logo] (l.png)");
        assert_eq!(t[4], "─".repeat(RULE_LEN));
        assert_eq!(t[6], "<div>brut</div>");
        assert_eq!(tok_of(&lines, 6, "div"), Some(Tok::Comment));
    }

    #[test]
    fn documents_limites_et_caracteres_de_controle() {
        assert_eq!(texts(""), ["(document vide)"]);
        assert_eq!(texts("\n\n"), ["(document vide)"]);
        // Les contrôles sont rendus visibles comme dans un fichier brut
        assert_eq!(texts("a\x1bb"), ["a\u{241b}b"]);
        // Les régions restent sur des frontières de caractères
        for l in render("# é **à** `日本` [ü](x)\n", false) {
            for r in &l.runs {
                assert!(l.text.is_char_boundary(r.start as usize));
                assert!(l.text.is_char_boundary(r.end as usize));
            }
        }
    }
}
