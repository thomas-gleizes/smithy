//! Souris et trackpad : le rendu dépose des zones cliquables, la boucle principale y cherche
//! ce qui se trouve sous le pointeur. Pur : ni terminal ni état de l'application.
//!
//! Seul le rendu sait où sont les choses (décalage d'une liste, onglets qui tiennent dans la
//! barre, fil d'Ariane abrégé) : il refait les zones à chaque image, comme `App::viewport`.

use std::cell::RefCell;

/// Ce qui se trouve sous le pointeur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Un onglet de la barre (indice dans la liste complète des onglets).
    Tab(usize),
    /// Une section de l'accueil (indice dans `Section::ALL`).
    Section(usize),
    /// Un segment du fil d'Ariane : la page de la pile (sa position) à retrouver ;
    /// `None` pour le serveur (ouvre la liste des serveurs).
    Crumb(Option<usize>),
    /// Une ligne de la liste de la page (indice dans la liste affichée).
    Row(usize),
    /// Une ligne de la liste ouverte par-dessus la page (onglets, serveurs, branches).
    PickerRow(usize),
    /// Une ligne d'écran d'un volet de texte (0 = la première affichée).
    TextRow(usize),
    /// Le cadre d'une fenêtre par-dessus la page, ou tout l'écran derrière un formulaire : un
    /// clic n'y fait rien (et n'atteint pas la page cachée dessous).
    Overlay,
    /// Autour d'une liste ouverte par-dessus la page : un clic la ferme, comme `Échap`.
    Outside,
}

#[derive(Debug, Clone, Copy)]
struct Zone {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    kind: Kind,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    /// Une cible unique sur toute la zone.
    Single(Target),
    /// Des lignes : `first` pour la ligne du haut, `count` lignes en tout à partir de `first`
    /// (au-delà, la zone est vide). `make` construit la cible de chaque ligne.
    Rows {
        first: usize,
        count: usize,
        make: fn(usize) -> Target,
    },
}

/// Les zones de l'image affichée ; la dernière déposée est au-dessus.
#[derive(Debug, Default)]
pub struct Zones {
    zones: RefCell<Vec<Zone>>,
}

/// Un rectangle à l'écran, sans dépendre de ratatui.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Zones {
    /// Une nouvelle image commence : les zones de la précédente ne valent plus.
    pub fn clear(&self) {
        self.zones.borrow_mut().clear();
    }

    pub fn add(&self, area: Area, target: Target) {
        self.push(area, Kind::Single(target));
    }

    /// Des lignes de liste : la ligne du haut de `area` montre l'élément `first`, et il y a
    /// `len` éléments en tout.
    pub fn rows(&self, area: Area, first: usize, len: usize, make: fn(usize) -> Target) {
        let count = len.saturating_sub(first);
        self.push(area, Kind::Rows { first, count, make });
    }

    fn push(&self, area: Area, kind: Kind) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.zones.borrow_mut().push(Zone {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
            kind,
        });
    }

    /// Ce qui est sous le point (`column`, `row`) : la zone la plus haute qui le contient.
    pub fn hit(&self, column: u16, row: u16) -> Option<Target> {
        let zones = self.zones.borrow();
        let zone = zones.iter().rev().find(|z| {
            column >= z.x
                && row >= z.y
                && u32::from(column) < u32::from(z.x) + u32::from(z.width)
                && u32::from(row) < u32::from(z.y) + u32::from(z.height)
        })?;
        match zone.kind {
            Kind::Single(t) => Some(t),
            Kind::Rows { first, count, make } => {
                let dy = usize::from(row - zone.y);
                // Sous la dernière ligne : rien, mais la zone masque quand même ce qui est dessous.
                (dy < count).then(|| make(first + dy))
            }
        }
    }
}

/// Lignes du fil d'Ariane abrégé (`fit_breadcrumb`) → indice du segment complet. Le serveur
/// reste en tête, « … » remplace des segments du milieu, la fin est conservée.
pub fn crumb_sources(kept: &[String], total: usize) -> Vec<Option<usize>> {
    let ellipsis = kept.len() >= 2 && kept[1] == "…" && kept.len() < total;
    kept.iter()
        .enumerate()
        .map(|(j, _)| {
            if !ellipsis {
                Some(j)
            } else if j == 0 {
                Some(0)
            } else if j == 1 {
                None
            } else {
                Some(total - (kept.len() - j))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: u16, y: u16, width: u16, height: u16) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn la_zone_du_dessus_l_emporte() {
        let z = Zones::default();
        z.add(area(0, 0, 80, 24), Target::Tab(0));
        z.add(area(10, 5, 20, 3), Target::Overlay);
        assert_eq!(z.hit(12, 6), Some(Target::Overlay));
        assert_eq!(z.hit(5, 6), Some(Target::Tab(0)));
        assert_eq!(z.hit(80, 0), None);
        z.clear();
        assert_eq!(z.hit(5, 6), None);
    }

    #[test]
    fn les_lignes_tiennent_compte_du_decalage_et_de_la_longueur() {
        let z = Zones::default();
        z.rows(area(1, 3, 40, 10), 7, 9, Target::Row);
        assert_eq!(z.hit(1, 3), Some(Target::Row(7)));
        assert_eq!(z.hit(5, 4), Some(Target::Row(8)));
        // Deux éléments seulement après le décalage : la suite de la zone est vide.
        assert_eq!(z.hit(5, 5), None);
        assert_eq!(z.hit(0, 3), None);
    }

    #[test]
    fn une_zone_vide_n_est_pas_retenue() {
        let z = Zones::default();
        z.add(area(0, 0, 0, 5), Target::Overlay);
        assert_eq!(z.hit(0, 0), None);
    }

    #[test]
    fn fil_d_ariane_abrege() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            crumb_sources(&s(&["srv", "Accueil", "a/b"]), 3),
            vec![Some(0), Some(1), Some(2)]
        );
        assert_eq!(
            crumb_sources(&s(&["srv", "…", "CI", "job"]), 6),
            vec![Some(0), None, Some(4), Some(5)]
        );
    }
}
