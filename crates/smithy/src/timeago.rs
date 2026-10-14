//! Dates relatives (« il y a 3 h ») sans dépendance : les serveurs donnent du RFC 3339.

use std::time::{SystemTime, UNIX_EPOCH};

/// Secondes depuis 1970, maintenant.
pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Jours depuis 1970 pour une date du calendrier grégorien (algorithme de H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-13T09:49:19Z` ou `…+02:00` → secondes depuis 1970. `None` si illisible.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        std::str::from_utf8(b.get(from..from + len)?)
            .ok()?
            .parse()
            .ok()
    };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Fraction de seconde éventuelle, puis fuseau.
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let offset = match b.get(i)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let (oh, om) = (num(i + 1, 2)?, num(i + 4, 2)?);
            let secs = oh * 3600 + om * 60;
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// « à l'instant », « il y a 5 min », « il y a 3 h », « il y a 2 j », « il y a 4 mois »…
pub fn ago(now: i64, then: i64) -> String {
    let s = now - then;
    if s < 0 {
        return "à l'instant".to_owned(); // horloge du serveur en avance
    }
    let (n, unit) = match s {
        0..=44 => return "à l'instant".to_owned(),
        45..=3_599 => ((s + 30) / 60, "min"),
        3_600..=86_399 => (s / 3_600, "h"),
        86_400..=2_591_999 => (s / 86_400, "j"),
        2_592_000..=31_535_999 => (s / 2_592_000, "mois"),
        _ => (
            s / 31_536_000,
            if s / 31_536_000 > 1 { "ans" } else { "an" },
        ),
    };
    format!("il y a {n} {unit}")
}

/// Date RFC 3339 → « il y a … » ; vide si elle est illisible.
pub fn ago_rfc3339(now: i64, s: &str) -> String {
    parse_rfc3339(s).map(|t| ago(now, t)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lecture_des_dates() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("1970-01-02T00:00:00Z"), Some(86_400));
        // Valeur de référence : 2026-10-13T09:49:19Z
        assert_eq!(parse_rfc3339("2026-10-13T09:49:19Z"), Some(1_791_884_959));
        // Fuseau et fraction de seconde
        assert_eq!(
            parse_rfc3339("2026-10-13T11:49:19+02:00"),
            Some(1_791_884_959)
        );
        assert_eq!(
            parse_rfc3339("2026-10-13T04:49:19-05:00"),
            Some(1_791_884_959)
        );
        assert_eq!(
            parse_rfc3339("2026-10-13T09:49:19.123456Z"),
            Some(1_791_884_959)
        );
        // Années bissextiles
        let feb29 = parse_rfc3339("2024-02-29T00:00:00Z").unwrap();
        let mar1 = parse_rfc3339("2024-03-01T00:00:00Z").unwrap();
        assert_eq!(mar1 - feb29, 86_400);
        assert_eq!(
            parse_rfc3339("2025-03-01T00:00:00Z").unwrap()
                - parse_rfc3339("2025-02-28T00:00:00Z").unwrap(),
            86_400
        );
        // Avant 1970 et illisibles
        assert!(parse_rfc3339("1969-12-31T23:59:59Z").unwrap() < 0);
        for bad in [
            "",
            "n'importe quoi",
            "2026-13-01T00:00:00Z",
            "2026-10-13",
            "2026-10-13T25:00:00Z",
            "2026-10-13T00:00:00",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn dates_relatives() {
        let now = 1_000_000_000;
        assert_eq!(ago(now, now), "à l'instant");
        assert_eq!(ago(now, now - 30), "à l'instant");
        assert_eq!(ago(now, now - 60), "il y a 1 min");
        assert_eq!(ago(now, now - 5 * 60), "il y a 5 min");
        assert_eq!(ago(now, now - 3_600), "il y a 1 h");
        assert_eq!(ago(now, now - 5 * 3_600), "il y a 5 h");
        assert_eq!(ago(now, now - 3 * 86_400), "il y a 3 j");
        assert_eq!(ago(now, now - 40 * 86_400), "il y a 1 mois");
        assert_eq!(ago(now, now - 400 * 86_400), "il y a 1 an");
        assert_eq!(ago(now, now - 800 * 86_400), "il y a 2 ans");
        assert_eq!(ago(now, now + 500), "à l'instant", "serveur en avance");
    }

    #[test]
    fn de_la_chaine_au_texte() {
        let then = parse_rfc3339("2026-10-13T09:49:19Z").unwrap();
        assert_eq!(
            ago_rfc3339(then + 2 * 3_600, "2026-10-13T09:49:19Z"),
            "il y a 2 h"
        );
        assert_eq!(ago_rfc3339(then, "illisible"), "");
        assert!(now_epoch() > 1_700_000_000);
    }
}
