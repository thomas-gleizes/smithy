//! Journal de smithy : un fichier texte, `~/.config/smithy/smithy.log`, où vont les actions
//! (démarrage, connexions, navigation, serveurs ajoutés ou retirés) et les erreurs (requêtes
//! refusées, session expirée, plantage). Une ligne par événement :
//!
//! ```text
//! 2026-10-22T13:37:30.123Z INFO  navigation : gl › Accueil › thomas-gleizes/smithy-test
//! ```
//!
//! `SMITHY_LOG` règle le niveau : `error` (erreurs seulement), `info` (par défaut : + actions),
//! `debug` (+ chaque requête réussie) ou `off`. Le fichier est en 0600 et tourne à 1 Mio
//! (`smithy.log.1` garde le précédent). Un secret (jeton, mot de passe) n'est jamais écrit à
//! dessein ; les motifs usuels sont de toute façon masqués (`redact`). Tout échec d'écriture est
//! ignoré : le journal ne doit jamais gêner l'application.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// Taille à partir de laquelle le fichier est mis de côté (`.1`) et un nouveau commencé.
const MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Info,
    Debug,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
        }
    }

    /// `SMITHY_LOG` : `None` pour `off`, `Some(niveau)` sinon (`info` si absent ou inconnu).
    fn from_env(value: Option<&str>) -> Option<Level> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("off" | "none" | "0") => None,
            Some("error" | "errors") => Some(Level::Error),
            Some("debug" | "trace") => Some(Level::Debug),
            _ => Some(Level::Info),
        }
    }
}

struct Sink {
    path: PathBuf,
    file: File,
    written: u64,
    /// Le dernier événement écrit et combien de fois il s'est répété depuis : un
    /// réseau coupé referait la même erreur toutes les 5 s.
    last: Option<(Level, String)>,
    repeats: u64,
}

impl Sink {
    fn append(&mut self, line: &str) {
        if self.written + line.len() as u64 > MAX_BYTES {
            rotate(&self.path);
            if let Ok(file) = open_file(&self.path) {
                self.file = file;
                self.written = 0;
            }
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += line.len() as u64;
        }
    }

    /// Écrit le compte des répétitions en attente, s'il y en a.
    fn flush_repeats(&mut self, at: SystemTime) {
        if self.repeats == 0 {
            return;
        }
        let Some(level) = self.last.as_ref().map(|(l, _)| *l) else {
            return;
        };
        let line = format!(
            "{} {} (message précédent répété {} fois)\n",
            timestamp(at),
            level.tag(),
            self.repeats
        );
        self.repeats = 0;
        self.append(&line);
    }
}

pub struct Logger {
    level: Level,
    sink: Mutex<Sink>,
}

fn open_file(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Met le fichier plein de côté (un seul ancien exemplaire est gardé).
fn rotate(path: &Path) {
    let mut old = path.as_os_str().to_owned();
    old.push(".1");
    let _ = fs::rename(path, PathBuf::from(old));
}

impl Logger {
    pub fn open(path: &Path, level: Level) -> io::Result<Self> {
        if fs::metadata(path).is_ok_and(|m| m.len() >= MAX_BYTES) {
            rotate(path);
        }
        let file = open_file(path)?;
        let written = file.metadata().map_or(0, |m| m.len());
        Ok(Self {
            level,
            sink: Mutex::new(Sink {
                path: path.to_owned(),
                file,
                written,
                last: None,
                repeats: 0,
            }),
        })
    }

    fn enabled(&self, level: Level) -> bool {
        level <= self.level
    }

    fn write_line(&self, at: SystemTime, level: Level, message: &str) {
        if !self.enabled(level) {
            return;
        }
        let text = clean(message);
        let Ok(mut sink) = self.sink.lock() else {
            return;
        };
        if sink
            .last
            .as_ref()
            .is_some_and(|(l, t)| *l == level && *t == text)
        {
            sink.repeats += 1;
            return;
        }
        sink.flush_repeats(at);
        let line = format!("{} {} {text}\n", timestamp(at), level.tag());
        sink.append(&line);
        sink.last = Some((level, text));
    }

    /// À la fermeture : n'oublie pas le dernier compte de répétitions.
    fn flush(&self, at: SystemTime) {
        if let Ok(mut sink) = self.sink.lock() {
            sink.flush_repeats(at);
        }
    }
}

/// `2026-10-22T13:37:30.123Z` (UTC : pas de fuseau horaire sans dépendance de plus).
fn timestamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (secs, millis) = (since.as_secs(), since.subsec_millis());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Jours depuis 1970-01-01 → (année, mois, jour) du calendrier grégorien (algorithme de H. Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Un événement tient sur une ligne : les sauts de ligne deviennent `⏎`, les autres caractères de
/// contrôle disparaissent, et les secrets usuels sont masqués.
fn clean(message: &str) -> String {
    let one_line: String = message
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' => Some('⏎'),
            '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    redact(&one_line)
}

/// Masque ce qui ressemble à un jeton : le mot qui suit `Bearer`, et les préfixes
/// connus (`glpat-`, `ghp_`, `gho_`, `github_pat_`).
fn redact(line: &str) -> String {
    const PREFIXES: [&str; 4] = ["glpat-", "ghp_", "gho_", "github_pat_"];
    let is_secret_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut out = String::with_capacity(line.len());
    let mut mask_next = false;
    for (i, word) in line.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // La ponctuation autour du mot (`(`, `)`, `,`) reste ; seul le cœur est masqué.
        let start = word.find(is_secret_char).unwrap_or(word.len());
        let end = word.rfind(is_secret_char).map_or(start, |i| i + 1);
        let (lead, bare, trail) = (&word[..start], &word[start..end], &word[end..]);
        if mask_next && !bare.is_empty() || PREFIXES.iter().any(|p| bare.starts_with(p)) {
            out.push_str(lead);
            out.push_str("***");
            out.push_str(trail);
        } else {
            out.push_str(word);
        }
        mask_next = bare == "Bearer";
    }
    out
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Où est le journal : à côté de `config.toml`.
pub fn path() -> Option<PathBuf> {
    crate::config::config_path().map(|p| p.with_file_name("smithy.log"))
}

/// Ouvre le journal (une seule fois). Sans dossier de configuration, ou fichier impossible à
/// ouvrir, rien n'est écrit.
pub fn init() {
    let Some(level) = Level::from_env(std::env::var("SMITHY_LOG").ok().as_deref()) else {
        return;
    };
    let Some(path) = path() else { return };
    if let Ok(logger) = Logger::open(&path, level) {
        let _ = LOGGER.set(logger);
    }
}

pub fn write(level: Level, args: fmt::Arguments) {
    if let Some(logger) = LOGGER.get()
        && logger.enabled(level)
    {
        logger.write_line(SystemTime::now(), level, &args.to_string());
    }
}

/// À la fermeture : écrit ce qui est encore en attente.
pub fn flush() {
    if let Some(logger) = LOGGER.get() {
        logger.flush(SystemTime::now());
    }
}

/// Un plantage va dans le journal avant d'être affiché.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let place = info
            .location()
            .map_or_else(String::new, |l| format!(" ({}:{})", l.file(), l.line()));
        let payload = info.payload();
        let what = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panique".to_owned());
        write(Level::Error, format_args!("plantage : {what}{place}"));
        previous(info);
    }));
}

macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Error, format_args!($($arg)*)) };
}
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Info, format_args!($($arg)*)) };
}
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Debug, format_args!($($arg)*)) };
}
pub(crate) use {debug, error, info};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("smithy-log-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(secs * 1000 + 123)
    }

    #[test]
    fn horodatage_en_utc() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        // 14:37:30 à Paris (UTC+2) le 22 octobre 2026
        assert_eq!(timestamp(at(1_792_672_650)), "2026-10-22T12:37:30.123Z");
        // Année bissextile : 29 février 2024, fin de journée
        assert_eq!(timestamp(at(1_709_251_199)), "2024-02-29T23:59:59.123Z");
        assert_eq!(timestamp(at(951_782_400)), "2000-02-29T00:00:00.123Z");
    }

    #[test]
    fn niveaux_de_smithy_log() {
        assert_eq!(Level::from_env(None), Some(Level::Info));
        assert_eq!(Level::from_env(Some("DEBUG")), Some(Level::Debug));
        assert_eq!(Level::from_env(Some(" error ")), Some(Level::Error));
        assert_eq!(Level::from_env(Some("off")), None);
        assert_eq!(Level::from_env(Some("n'importe quoi")), Some(Level::Info));
        assert!(Level::Error < Level::Info && Level::Info < Level::Debug);
    }

    #[test]
    fn une_ligne_par_evenement_filtree_par_niveau() {
        let tmp = Tmp::new("niveaux");
        let path = tmp.0.join("smithy.log");
        let log = Logger::open(&path, Level::Info).unwrap();
        log.write_line(at(0), Level::Info, "navigation : a › b");
        log.write_line(at(1), Level::Debug, "requête ignorée à ce niveau");
        log.write_line(at(2), Level::Error, "ligne un\nligne deux\x1b[31m");
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "1970-01-01T00:00:00.123Z INFO  navigation : a › b",
                "1970-01-01T00:00:02.123Z ERROR ligne un⏎ligne deux[31m",
            ]
        );
        // Un second démarrage ajoute à la suite
        let again = Logger::open(&path, Level::Debug).unwrap();
        again.write_line(at(3), Level::Debug, "suite");
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 3);
    }

    #[test]
    fn une_erreur_repetee_n_est_ecrite_qu_une_fois() {
        let tmp = Tmp::new("repetition");
        let path = tmp.0.join("smithy.log");
        let log = Logger::open(&path, Level::Info).unwrap();
        for i in 0..4 {
            log.write_line(at(i), Level::Error, "réseau : connexion impossible");
        }
        log.write_line(at(9), Level::Info, "autre chose");
        // Le même texte à un autre niveau n'est pas une répétition
        log.write_line(at(10), Level::Error, "autre chose");
        log.write_line(at(11), Level::Error, "autre chose");
        log.flush(at(12));
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        assert!(lines[0].ends_with("ERROR réseau : connexion impossible"));
        assert!(lines[1].ends_with("répété 3 fois)"), "{}", lines[1]);
        assert!(lines[2].ends_with("INFO  autre chose"));
        assert!(lines[3].ends_with("ERROR autre chose"));
        assert!(lines[4].ends_with("répété 1 fois)"));
    }

    #[test]
    fn les_secrets_sont_masques() {
        assert_eq!(
            redact("Authorization: Bearer abc123 ok"),
            "Authorization: Bearer *** ok"
        );
        assert_eq!(redact("en-tête ghp_xyz fin"), "en-tête *** fin");
        assert_eq!(redact("jeton glpat-AbC_9 et gho_zzz"), "jeton *** et ***");
        assert_eq!(
            redact("HTTP 401 sur /user : non authentifié"),
            "HTTP 401 sur /user : non authentifié"
        );
        // Le mot « token » d'un message d'erreur n'est pas un secret
        assert_eq!(
            redact("non authentifié (token absent ou invalide)"),
            "non authentifié (token absent ou invalide)"
        );
        assert_eq!(redact("le Bearer"), "le Bearer");
        assert_eq!(redact("(Bearer tok123)"), "(Bearer ***)");
    }

    #[cfg(unix)]
    #[test]
    fn fichier_prive_et_rotation() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = Tmp::new("rotation");
        let path = tmp.0.join("sous").join("smithy.log");
        let log = Logger::open(&path, Level::Info).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Remplit le fichier : le suivant tourne vers `.1`
        let gros = "x".repeat(4000);
        // Lignes toutes différentes : les répétitions ne seraient pas écrites
        for i in 0..(MAX_BYTES / 4000 + 5) {
            log.write_line(at(0), Level::Info, &format!("{i} {gros}"));
        }
        let old = path.with_file_name("smithy.log.1");
        assert!(old.exists(), "l'ancien exemplaire est gardé");
        assert!(fs::metadata(&path).unwrap().len() < MAX_BYTES);
        assert!(fs::metadata(&old).unwrap().len() <= MAX_BYTES);
        // Au démarrage, un fichier déjà plein tourne aussi
        fs::write(&path, vec![b'y'; MAX_BYTES as usize]).unwrap();
        Logger::open(&path, Level::Info).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(fs::metadata(&old).unwrap().len(), MAX_BYTES);
    }
}
