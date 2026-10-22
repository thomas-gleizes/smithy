//! Découpe la trace d'un job GitLab en étapes. La trace est un seul texte, sans statut par étape ;
//! seuls des marqueurs écrits par le runner la structurent (mesurés, voir CLAUDE.md) :
//!
//! - chaque ligne est préfixée `2026-10-22T12:41:59.601590Z 01O ` (horodatage, flux, `O` ou `E`),
//!   préfixe **facultatif** selon l'instance ; un `+` à la place de l'espace après le flux
//!   (`00O+`) dit que la ligne **continue la précédente**, qui n'a pas de saut de ligne : on
//!   recolle avant de lire quoi que ce soit ;
//! - `section_start:<ts>:<nom>[options]\r\e[0K` ouvre une section, dont le titre affiché suit sur
//!   la même ligne (une fois recollée) ; `section_end:<ts>:<nom>\r\e[0K` la ferme. Plusieurs
//!   marqueurs peuvent tenir sur une même ligne recollée, et les sections s'imbriquent ;
//! - l'échec (`ERROR: Job failed: …`) est écrit **hors de toute section**, après le dernier
//!   `section_end`, donc l'étape en échec est à déduire : `step_script` (le script du job), sinon
//!   la dernière étape qui n'est pas un nettoyage.
//!
//! Chaque section devient une étape, dans l'ordre d'ouverture, avec les lignes qui lui reviennent
//! tant qu'aucune section fille n'est ouverte. Ce qui précède la première section (en-tête du
//! runner) et ce qui suit la dernière (résultat du job) forment deux étapes à part.

use smithy_core::{JobLog, Status, StepLog, clean_log_line};

/// Étapes d'après le script, que le runner joue après lui : elles ne portent pas l'échec du job.
const CLEANUP: &[&str] = &[
    "cleanup_file_variables",
    "upload_artifacts_on_success",
    "upload_artifacts_on_failure",
    "archive_cache",
    "archive_cache_on_failure",
    "after_script",
];

/// Retire le préfixe `<horodatage> <NN><flux><espace|+>` ; renvoie le contenu et si la ligne
/// continue la précédente. Sans préfixe reconnu, la ligne est rendue telle quelle.
fn split_prefix(line: &str) -> (&str, bool) {
    let b = line.as_bytes();
    let dated = b.len() > 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[..4].iter().all(u8::is_ascii_digit);
    if !dated {
        return (line, false);
    }
    let Some(z) = b.iter().take(40).position(|&c| c == b'Z') else {
        return (line, false);
    };
    let ok = b.get(z + 1) == Some(&b' ')
        && b.get(z + 2).is_some_and(u8::is_ascii_digit)
        && b.get(z + 3).is_some_and(u8::is_ascii_digit)
        && matches!(b.get(z + 4), Some(b'O' | b'E'));
    if !ok {
        return (line, false);
    }
    match b.get(z + 5) {
        None => ("", false),
        Some(b' ') => (&line[z + 6..], false),
        Some(b'+') => (&line[z + 6..], true),
        Some(_) => (line, false),
    }
}

/// Les lignes de la trace, les continuations recollées.
fn logical_lines(text: &str) -> Vec<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let (content, continues) = split_prefix(raw);
        match out.last_mut() {
            Some(last) if continues => last.push_str(content),
            _ => out.push(content.to_owned()),
        }
    }
    // Le texte finit par un saut de ligne : pas de ligne vide en plus.
    if out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

enum Seg<'a> {
    Text(&'a str),
    /// Nom de la section sans ses options (`analyse[collapsed=true]` → `analyse`).
    Start(&'a str),
    End(&'a str),
}

/// Sépare une ligne en texte et marqueurs de section.
fn segments(line: &str) -> Vec<Seg<'_>> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut text_from = 0;
    while let Some(rel) = line[from..].find("section_") {
        let at = from + rel;
        let rest = &line[at + "section_".len()..];
        let (start, tag_len) = if rest.starts_with("start:") {
            (true, "start:".len())
        } else if rest.starts_with("end:") {
            (false, "end:".len())
        } else {
            from = at + 1;
            continue;
        };
        let after_tag = &rest[tag_len..];
        let digits = after_tag.bytes().take_while(u8::is_ascii_digit).count();
        let named = after_tag[digits..].strip_prefix(':');
        let Some(named) = named.filter(|_| digits > 0) else {
            from = at + 1;
            continue;
        };
        let name_len = named.find(['\r', '\x1b', '\n']).unwrap_or(named.len());
        let (name, mut tail) = named.split_at(name_len);
        if name.is_empty() {
            from = at + 1;
            continue;
        }
        tail = tail.strip_prefix('\r').unwrap_or(tail);
        tail = tail.strip_prefix("\x1b[0K").unwrap_or(tail);
        if at > text_from {
            out.push(Seg::Text(&line[text_from..at]));
        }
        let base = name.split('[').next().unwrap_or(name);
        out.push(if start {
            Seg::Start(base)
        } else {
            Seg::End(base)
        });
        // Ce qui reste à lire commence après le marqueur.
        text_from = line.len() - tail.len();
        from = text_from;
    }
    if text_from < line.len() {
        out.push(Seg::Text(&line[text_from..]));
    }
    out
}

struct Section {
    key: String,
    title: Option<String>,
    lines: Vec<String>,
    closed: bool,
}

/// Découpe `text` en étapes ; `status` est celui du job (il dit quelle étape a échoué).
pub(crate) fn split(text: &str, status: Status) -> JobLog {
    let mut sections: Vec<Section> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let (mut before, mut after) = (Vec::new(), Vec::new());
    let mut all = Vec::new();

    for line in logical_lines(text) {
        let segs = segments(&line);
        if !segs.iter().any(|s| !matches!(s, Seg::Text(_))) {
            let shown = clean_log_line(&line);
            all.push(shown.clone());
            match stack.last() {
                Some(&i) => sections[i].lines.push(shown),
                None if sections.is_empty() => before.push(shown),
                None => after.push(shown),
            }
            continue;
        }
        // Le titre d'une section est le premier texte qui suit son marqueur, sur la même ligne.
        let mut awaiting_title = None;
        for seg in segs {
            match seg {
                Seg::Start(key) => {
                    sections.push(Section {
                        key: key.to_owned(),
                        title: None,
                        lines: Vec::new(),
                        closed: false,
                    });
                    stack.push(sections.len() - 1);
                    awaiting_title = Some(sections.len() - 1);
                }
                Seg::End(key) => {
                    if let Some(pos) = stack.iter().rposition(|&i| sections[i].key == key) {
                        for i in stack.drain(pos..) {
                            sections[i].closed = true;
                        }
                    }
                    awaiting_title = None;
                }
                Seg::Text(t) => {
                    let shown = clean_log_line(t);
                    if shown.trim().is_empty() {
                        continue;
                    }
                    all.push(shown.clone());
                    match (awaiting_title.take(), stack.last()) {
                        (Some(i), _) if sections[i].title.is_none() => {
                            sections[i].title = Some(shown.trim().to_owned());
                        }
                        (_, Some(&i)) => sections[i].lines.push(shown),
                        (_, None) if sections.is_empty() => before.push(shown),
                        (_, None) => after.push(shown),
                    }
                }
            }
        }
    }

    if sections.is_empty() {
        // Pas de marqueur (ancien runner) : tout le log dans une seule section.
        let steps = if all.is_empty() {
            Vec::new()
        } else {
            vec![StepLog {
                name: "Journal complet".to_owned(),
                status,
                lines: all,
            }]
        };
        return JobLog {
            steps,
            notice: None,
        };
    }

    // L'étape en échec : une section jamais fermée si le job s'est arrêté dedans, sinon le script.
    let failed = (status == Status::Failure && !sections.iter().any(|s| !s.closed))
        .then(|| {
            sections
                .iter()
                .position(|s| s.key == "step_script")
                .or_else(|| {
                    sections
                        .iter()
                        .rposition(|s| !CLEANUP.contains(&s.key.as_str()))
                })
        })
        .flatten();

    let mut steps = Vec::new();
    if !before.is_empty() {
        steps.push(StepLog {
            name: "Démarrage du job".to_owned(),
            status: Status::Success,
            lines: before,
        });
    }
    for (i, s) in sections.into_iter().enumerate() {
        let step_status = if Some(i) == failed {
            Status::Failure
        } else if s.closed {
            Status::Success
        } else {
            match status {
                Status::Running | Status::Pending => Status::Running,
                Status::Failure | Status::Cancelled => status,
                _ => Status::Unknown,
            }
        };
        steps.push(StepLog {
            name: s.title.unwrap_or(s.key),
            status: step_status,
            lines: s.lines,
        });
    }
    if !after.is_empty() {
        steps.push(StepLog {
            name: "Fin du job".to_owned(),
            status: match status {
                Status::Running | Status::Pending => Status::Running,
                s => s,
            },
            lines: after,
        });
    }
    JobLog {
        steps,
        notice: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINT: &str = include_str!("../tests/fixtures/trace_lint.log");
    const FAILING: &str = include_str!("../tests/fixtures/trace_failing.log");
    const FLAKY: &str = include_str!("../tests/fixtures/trace_flaky.log");
    const COMPILE: &str = include_str!("../tests/fixtures/trace_compile.log");

    fn names(log: &JobLog) -> Vec<&str> {
        log.steps.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn prefixe_et_continuations() {
        assert_eq!(
            split_prefix("2026-10-22T12:41:59.601590Z 01O texte"),
            ("texte", false)
        );
        assert_eq!(
            split_prefix("2026-10-22T12:41:59.601663Z 00O+suite"),
            ("suite", true)
        );
        assert_eq!(split_prefix("2026-10-22T12:41:59.6Z 01E "), ("", false));
        // Sans préfixe (autre instance) ou préfixe qui n'en est pas un : intact
        assert_eq!(split_prefix("texte brut"), ("texte brut", false));
        assert_eq!(
            split_prefix("2026-10-22 est une date dans le texte"),
            ("2026-10-22 est une date dans le texte", false)
        );
        let lines = logical_lines(
            "2026-10-22T12:00:00.1Z 00O a\n2026-10-22T12:00:00.2Z 00O+b\n2026-10-22T12:00:00.3Z 01O c\n",
        );
        assert_eq!(lines, ["ab", "c"]);
    }

    #[test]
    fn marqueurs_options_et_marqueurs_multiples() {
        let line = "avant section_end:1:a\r\x1b[0Ksection_start:2:b[collapsed=true]\r\x1b[0K\x1b[36mTitre\x1b[0m";
        let segs = segments(line);
        assert!(matches!(segs[0], Seg::Text("avant ")));
        assert!(matches!(segs[1], Seg::End("a")));
        assert!(matches!(segs[2], Seg::Start("b")), "options retirées");
        assert!(matches!(segs[3], Seg::Text(t) if t.contains("Titre")));
        // Pas un marqueur : pas de nombre, pas de nom
        assert!(matches!(
            segments("section_start:x:y").as_slice(),
            [Seg::Text(_)]
        ));
        assert!(matches!(
            segments("la section_end: finie").as_slice(),
            [Seg::Text(_)]
        ));
    }

    #[test]
    fn job_reussi_avec_section_de_l_utilisateur() {
        let log = split(LINT, Status::Success);
        assert_eq!(
            names(&log),
            [
                "Démarrage du job",
                "Preparing the \"docker+machine\" executor",
                "Preparing environment",
                "Getting source from Git repository",
                "Executing \"step_script\" stage of the job script",
                "Analyse",
                "Cleaning up project directory and file based variables",
                "Fin du job",
            ]
        );
        assert!(log.steps.iter().all(|s| s.status == Status::Success));
        let analyse = log.steps.iter().find(|s| s.name == "Analyse").unwrap();
        assert_eq!(
            analyse.lines.len(),
            15,
            "section posée par le script du job"
        );
        assert_eq!(analyse.lines[0], "fichier 1 ok");
        let script = &log.steps[4].lines;
        assert!(script.iter().any(|l| l == "vert rouge gras"), "ANSI retiré");
        assert!(script.iter().any(|l| l == "lint terminé"), "UTF-8 conservé");
        assert!(
            !script
                .iter()
                .any(|l| l.contains("section_start:17") || l.contains("section_end:")),
            "aucun marqueur affiché (la commande `echo` qui en écrit un reste du texte)"
        );
        assert_eq!(log.steps[7].lines, ["Job succeeded"]);
        assert!(log.steps[0].lines[0].starts_with("Running with gitlab-runner"));
    }

    #[test]
    fn job_en_echec_marque_le_script_pas_le_nettoyage() {
        let log = split(FAILING, Status::Failure);
        let status = |n: &str| {
            log.steps
                .iter()
                .find(|s| s.name.contains(n))
                .unwrap()
                .status
        };
        assert_eq!(status("step_script"), Status::Failure);
        assert_eq!(status("Getting source"), Status::Success);
        assert_eq!(status("Cleaning up"), Status::Success);
        let fin = log.steps.last().unwrap();
        assert_eq!(fin.name, "Fin du job");
        assert_eq!(fin.status, Status::Failure);
        assert_eq!(fin.lines, ["ERROR: Job failed: exit code 1"]);
        let script = log
            .steps
            .iter()
            .find(|s| s.name.contains("step_script"))
            .unwrap();
        assert!(
            script.lines.iter().any(|l| l == "erreur simulée"),
            "flux stderr"
        );
        let first_failure = log.steps.iter().position(|s| s.status == Status::Failure);
        assert_eq!(
            first_failure,
            log.steps
                .iter()
                .position(|s| s.name.contains("step_script"))
        );
    }

    #[test]
    fn grosse_sortie_et_echec_tolere() {
        let compile = split(COMPILE, Status::Success);
        let script = compile
            .steps
            .iter()
            .find(|s| s.name.contains("step_script"))
            .unwrap();
        assert!(script.lines.iter().any(|l| l == "ligne 40"));
        let flaky = split(FLAKY, Status::Failure);
        assert_eq!(
            flaky
                .steps
                .iter()
                .filter(|s| s.status == Status::Failure)
                .count(),
            2,
            "le script et le résultat"
        );
    }

    #[test]
    fn job_en_cours_derniere_section_ouverte() {
        // Coupée au milieu du script : la section n'est pas fermée.
        let cut = LINT.split("section_end:1792672927:analyse").next().unwrap();
        let log = split(cut, Status::Running);
        let last = log.steps.last().unwrap();
        assert_eq!(last.name, "Analyse");
        assert_eq!(last.status, Status::Running);
        assert!(
            log.steps[..log.steps.len() - 1]
                .iter()
                .any(|s| s.status == Status::Running),
            "le script englobant aussi"
        );
        assert_eq!(log.steps[1].status, Status::Success);
        // Tué en route : les sections ouvertes prennent le statut du job
        let killed = split(cut, Status::Cancelled);
        assert_eq!(killed.steps.last().unwrap().status, Status::Cancelled);
    }

    #[test]
    fn sans_horodatage_ni_continuation_meme_decoupage() {
        // Autre instance : le préfixe est absent ; les lignes ne sont plus recollables, mais
        // les marqueurs seuls suffisent à reconnaître les sections.
        let bare: String = LINT
            .lines()
            .map(|l| format!("{}\n", split_prefix(l).0))
            .collect();
        let log = split(&bare, Status::Success);
        assert!(
            names(&log).contains(&"Analyse") && names(&log).contains(&"Démarrage du job"),
            "{:?}",
            names(&log)
        );
        assert_eq!(
            log.steps
                .iter()
                .find(|s| s.name == "Analyse")
                .unwrap()
                .lines
                .len(),
            15
        );
    }

    #[test]
    fn sections_imbriquees_et_trace_sans_marqueur() {
        let nested = "section_start:1:ext\r\x1b[0KExterne\nA\nsection_start:2:int\r\x1b[0KInterne\nB\nsection_end:2:int\r\x1b[0K\nC\nsection_end:1:ext\r\x1b[0K\nfin\n";
        let log = split(nested, Status::Success);
        assert_eq!(names(&log), ["Externe", "Interne", "Fin du job"]);
        assert_eq!(
            log.steps[0].lines,
            ["A", "C"],
            "la ligne après la fille revient à la mère"
        );
        assert_eq!(log.steps[1].lines, ["B"]);
        let plain = split("une\ndeux\n", Status::Failure);
        assert_eq!(names(&plain), ["Journal complet"]);
        assert_eq!(plain.steps[0].status, Status::Failure);
        assert!(split("", Status::Success).steps.is_empty());
    }
}
