//! File d'attente : historique, morceau en cours, suite, shuffle et repeat.
//!
//! En clair : trois morceaux de liste bout à bout.
//! - l'**historique** : ce qui a vraiment été joué (pour « précédent ») ;
//!   il n'est pas montré dans la file ;
//! - le **morceau en cours** ;
//! - la **suite** : ce qui va être joué, dans l'ordre.
//!
//! À côté, le **contexte** garde la liste d'origine (la playlist lancée, plus
//! les ajouts). Lancer un morceau au milieu d'une playlist ne marque rien comme
//! « déjà joué » : la suite contient tout le reste de la playlist (la fin, puis
//! le début). Activer ou couper l'aléatoire reconstruit la suite à partir du
//! contexte entier : on peut toujours réécouter toute la playlist.
//!
//! Chaque entrée reçoit un identifiant unique ([`QueueId`]), pour qu'on puisse
//! retirer ou déplacer « ce morceau-là » même si le même titre apparaît deux fois.
//! Ce module ne fait aucune entrée/sortie : il est entièrement testé à part.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::Track;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueueId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedTrack {
    pub id: QueueId,
    pub track: Track,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    /// Reprend toute la liste après le dernier morceau.
    All,
    /// Rejoue le morceau courant en boucle (seulement en enchaînement automatique).
    One,
}

impl Repeat {
    /// Off → All → One → Off, comme le bouton de YouTube Music.
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// Ce qu'a provoqué un retrait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed {
    NotFound,
    Upcoming,
    /// Le morceau en cours a été retiré : il n'y a plus de morceau courant.
    Current,
}

/// Historique gardé pour « précédent ».
const MAX_HISTORY: usize = 200;

pub struct Queue {
    /// Historique, puis morceau en cours, puis suite.
    entries: Vec<QueuedTrack>,
    current: Option<usize>,
    /// Sans morceau en cours : début de la suite dans `entries`.
    resume: usize,
    /// Liste d'origine (playlist lancée + ajouts), dans son ordre.
    context: Vec<Track>,
    /// Entrée → position dans `context`.
    origin: HashMap<QueueId, usize>,
    shuffle: bool,
    repeat: Repeat,
    next_id: u64,
    rng: fastrand::Rng,
}

impl Default for Queue {
    fn default() -> Self {
        Self::with_rng(fastrand::Rng::new())
    }
}

impl Queue {
    pub fn with_rng(rng: fastrand::Rng) -> Self {
        Self {
            entries: Vec::new(),
            current: None,
            resume: 0,
            context: Vec::new(),
            origin: HashMap::new(),
            shuffle: false,
            repeat: Repeat::Off,
            next_id: 1,
            rng,
        }
    }

    // ----- lecture -----

    /// Morceau en cours puis suite : ce que la file affiche (sans l'historique).
    pub fn visible(&self) -> &[QueuedTrack] {
        &self.entries[self.current.unwrap_or(self.resume).min(self.entries.len())..]
    }

    /// Historique, courant et suite (pour la session).
    pub fn entries(&self) -> &[QueuedTrack] {
        &self.entries
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<&QueuedTrack> {
        self.current.map(|i| &self.entries[i])
    }

    pub fn get(&self, id: QueueId) -> Option<&QueuedTrack> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn contains_video(&self, video_id: &str) -> bool {
        self.entries.iter().any(|e| e.track.video_id == video_id)
            || self.context.iter().any(|t| t.video_id == video_id)
    }

    fn upcoming_start(&self) -> usize {
        self.current
            .map_or(self.resume, |i| i + 1)
            .min(self.entries.len())
    }

    /// Morceaux après le courant, dans l'ordre de lecture.
    pub fn upcoming(&self) -> &[QueuedTrack] {
        &self.entries[self.upcoming_start()..]
    }

    pub fn upcoming_len(&self) -> usize {
        self.entries.len() - self.upcoming_start()
    }

    // ----- construction -----

    fn fresh(&mut self, track: Track, origin: Option<usize>) -> QueuedTrack {
        let id = QueueId(self.next_id);
        self.next_id += 1;
        if let Some(o) = origin {
            self.origin.insert(id, o);
        }
        QueuedTrack { id, track }
    }

    fn current_origin(&self) -> Option<usize> {
        self.current().and_then(|e| self.origin.get(&e.id).copied())
    }

    /// Positions du contexte à jouer après `from` : la fin de la liste puis
    /// son début (ordre d'origine), ou tout le reste mélangé.
    fn order_after(&mut self, from: Option<usize>) -> Vec<usize> {
        let len = self.context.len();
        let mut order: Vec<usize> = match from {
            Some(f) => (f + 1..len).chain(0..f.min(len)).collect(),
            None => (0..len).collect(),
        };
        // Jamais le morceau en cours dans sa propre suite : il peut figurer deux
        // fois dans le contexte (ajouté de nouveau, radio) ou ne pas y avoir de
        // place connue ; il revenait alors en tête d'une file vide passée en
        // aléatoire.
        if let Some(current) = self.current().map(|e| e.track.video_id.clone()) {
            order.retain(|&o| self.context[o].video_id != current);
        }
        if self.shuffle {
            self.rng.shuffle(&mut order);
        }
        order
    }

    fn push_from_context(&mut self, order: Vec<usize>) {
        for o in order {
            let entry = self.fresh(self.context[o].clone(), Some(o));
            self.entries.push(entry);
        }
    }

    /// Remplace la suite par le reste du contexte (dans l'ordre courant).
    fn rebuild_upcoming(&mut self) {
        let start = self.upcoming_start();
        let removed: Vec<QueueId> = self.entries.drain(start..).map(|e| e.id).collect();
        for id in removed {
            self.origin.remove(&id);
        }
        let order = self.order_after(self.current_origin());
        self.push_from_context(order);
    }

    /// Repeat::All : la liste repart quand la suite est vide.
    fn top_up(&mut self) {
        if self.repeat == Repeat::All && self.upcoming_len() == 0 && !self.context.is_empty() {
            let from = self.current_origin();
            let mut order = self.order_after(from);
            if let Some(f) = from {
                // Le tour complet inclut le morceau en cours, à la fin.
                order.push(f);
            }
            self.push_from_context(order);
        }
    }

    /// Garde l'historique sous `MAX_HISTORY`.
    fn trim_history(&mut self) {
        let history = self.current.unwrap_or(self.resume);
        if history > MAX_HISTORY {
            let drop = history - MAX_HISTORY;
            let removed: Vec<QueueId> = self.entries.drain(..drop).map(|e| e.id).collect();
            for id in removed {
                self.origin.remove(&id);
            }
            self.current = self.current.map(|c| c - drop);
            self.resume -= drop.min(self.resume);
        }
    }

    /// Nouvelle liste ; `tracks[start]` devient le morceau en cours. Rien n'est
    /// considéré comme joué : la suite contient tous les autres titres.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) -> Option<QueueId> {
        self.entries.clear();
        self.origin.clear();
        self.current = None;
        self.resume = 0;
        self.context = tracks;
        if self.context.is_empty() {
            return None;
        }
        let start = start.min(self.context.len() - 1);
        let entry = self.fresh(self.context[start].clone(), Some(start));
        let id = entry.id;
        self.entries.push(entry);
        self.current = Some(0);
        self.rebuild_upcoming();
        self.top_up();
        Some(id)
    }

    /// Comme [`Queue::replace`], aléatoire activé et départ tiré au hasard.
    pub fn replace_shuffled(&mut self, tracks: Vec<Track>) -> Option<QueueId> {
        self.shuffle = true;
        let start = if tracks.is_empty() {
            0
        } else {
            self.rng.usize(..tracks.len())
        };
        self.replace(tracks, start)
    }

    /// Session reprise : `tracks[..current]` est l'historique, le reste la suite
    /// telle quelle (ordre gardé, même mélangé).
    pub fn restore(
        &mut self,
        tracks: Vec<Track>,
        current: usize,
        shuffle: bool,
    ) -> Option<QueueId> {
        self.entries.clear();
        self.origin.clear();
        self.shuffle = shuffle;
        self.current = None;
        self.resume = 0;
        self.context = tracks.clone();
        if tracks.is_empty() {
            return None;
        }
        let current = current.min(tracks.len() - 1);
        for (i, track) in tracks.into_iter().enumerate() {
            let entry = self.fresh(track, Some(i));
            self.entries.push(entry);
        }
        self.current = Some(current);
        self.top_up();
        self.current().map(|e| e.id)
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
        self.top_up();
    }

    /// Active ou coupe l'aléatoire : la suite est reconstruite avec **toute** la
    /// liste (sauf le morceau en cours), mélangée ou dans l'ordre d'origine.
    /// L'historique ne bouge pas.
    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle = on;
        self.rebuild_upcoming();
        self.top_up();
    }

    // ----- ajouts -----

    /// Ajoute à la fin (au hasard dans la suite en mode aléatoire).
    pub fn append(&mut self, tracks: Vec<Track>) -> Vec<QueueId> {
        let mut ids = Vec::with_capacity(tracks.len());
        for track in tracks {
            self.context.push(track.clone());
            let entry = self.fresh(track, Some(self.context.len() - 1));
            ids.push(entry.id);
            let len = self.entries.len();
            // Le tout prochain (déjà préchargé) ne bouge pas.
            let first_free = (self.upcoming_start() + 1).min(len);
            let at = if self.shuffle && first_free < len {
                self.rng.usize(first_free..=len)
            } else {
                len
            };
            self.entries.insert(at, entry);
        }
        ids
    }

    /// Ajout de la radio (même règle que [`Queue::append`]).
    pub fn append_mixed(&mut self, tracks: Vec<Track>) -> Vec<QueueId> {
        self.append(tracks)
    }

    /// Insère juste après le morceau courant (« lire ensuite »).
    pub fn insert_next(&mut self, tracks: Vec<Track>) -> Vec<QueueId> {
        let ctx_at = self.current_origin().map_or(self.context.len(), |o| o + 1);
        let count = tracks.len();
        // Les positions du contexte après l'insertion se décalent.
        for o in self.origin.values_mut() {
            if *o >= ctx_at {
                *o += count;
            }
        }
        let at = self.upcoming_start();
        let mut ids = Vec::with_capacity(count);
        for (k, track) in tracks.into_iter().enumerate() {
            self.context.insert(ctx_at + k, track.clone());
            let entry = self.fresh(track, Some(ctx_at + k));
            ids.push(entry.id);
            self.entries.insert(at + k, entry);
        }
        ids
    }

    // ----- retraits et déplacements -----

    /// Retire une entrée ; elle disparaît aussi de la liste d'origine (elle ne
    /// reviendra pas en changeant de mode).
    pub fn remove(&mut self, id: QueueId) -> Removed {
        let Some(index) = self.index_of(id) else {
            return Removed::NotFound;
        };
        self.entries.remove(index);
        if let Some(o) = self.origin.remove(&id) {
            self.context.remove(o);
            self.origin.retain(|_, x| *x != o);
            for x in self.origin.values_mut() {
                if *x > o {
                    *x -= 1;
                }
            }
        }
        match self.current {
            Some(cur) if cur == index => {
                self.current = None;
                self.resume = index;
                Removed::Current
            }
            Some(cur) if index < cur => {
                self.current = Some(cur - 1);
                Removed::Upcoming
            }
            None if index < self.resume => {
                self.resume -= 1;
                Removed::Upcoming
            }
            _ => Removed::Upcoming,
        }
    }

    /// Déplace une entrée à la position `to` de la file visible (0 = morceau en
    /// cours). Le morceau en cours reste en cours.
    pub fn move_to(&mut self, id: QueueId, to: usize) -> bool {
        let Some(from) = self.index_of(id) else {
            return false;
        };
        let base = self.current.unwrap_or(self.resume);
        if from < base {
            return false; // historique : invisible, immobile
        }
        let current_id = self.current().map(|e| e.id);
        let entry = self.entries.remove(from);
        let to = (base + to).clamp(base, self.entries.len());
        self.entries.insert(to, entry);
        self.current = current_id.and_then(|cid| self.index_of(cid));
        true
    }

    /// Vide la suite : seul le morceau en cours reste (et l'historique).
    pub fn clear_upcoming(&mut self) {
        let start = self.upcoming_start();
        self.entries.truncate(start);
        // Changer de mode ne doit pas faire revenir la liste vidée.
        let current = self.current().map(|e| (e.id, e.track.clone()));
        self.context.clear();
        self.origin.clear();
        if let Some((id, track)) = current {
            self.context.push(track);
            self.origin.insert(id, 0);
        }
    }

    // ----- déplacements du curseur -----

    /// Fait de `id` le morceau en cours.
    /// - En avant : le courant passe dans l'historique, les morceaux sautés
    ///   quittent la suite (ils restent dans la liste d'origine).
    /// - En arrière (précédent) : les morceaux d'après repassent dans la suite.
    pub fn set_current(&mut self, id: QueueId) -> bool {
        let Some(target) = self.index_of(id) else {
            return false;
        };
        let start = self.upcoming_start();
        if target >= start {
            let skipped: Vec<QueueId> = self.entries.drain(start..target).map(|e| e.id).collect();
            for id in skipped {
                self.origin.remove(&id);
            }
            self.current = Some(start);
        } else {
            self.current = Some(target);
        }
        self.trim_history();
        self.top_up();
        true
    }

    /// Ce qui sera joué à la fin naturelle du morceau courant (sans rien changer).
    pub fn peek_auto_next(&self) -> Option<&QueuedTrack> {
        let cur = self.current?;
        if self.repeat == Repeat::One {
            return Some(&self.entries[cur]);
        }
        self.entries.get(cur + 1)
    }

    /// Fin naturelle du morceau : avance et rend le nouveau courant.
    pub fn advance_auto(&mut self) -> Option<QueueId> {
        let id = self.peek_auto_next()?.id;
        self.set_current(id);
        Some(id)
    }

    /// Bouton « suivant » : ignore Repeat::One.
    pub fn next_manual(&self) -> Option<QueueId> {
        self.entries.get(self.upcoming_start()).map(|e| e.id)
    }

    /// Bouton « précédent » : dernier morceau vraiment joué.
    pub fn previous(&self) -> Option<QueueId> {
        let before = self.current.unwrap_or(self.resume).checked_sub(1)?;
        self.entries.get(before).map(|e| e.id)
    }

    /// « Précédent » : le dernier morceau joué ; sans historique (lancé au
    /// milieu d'une playlist), le titre d'avant dans la liste d'origine, qui
    /// quitte alors la fin de la suite.
    pub fn prepare_previous(&mut self) -> Option<QueueId> {
        if let Some(id) = self.previous() {
            return Some(id);
        }
        let cur = self.current?;
        if self.shuffle {
            return None;
        }
        let before = self.current_origin()?.checked_sub(1)?;
        let duplicate = self.entries[cur + 1..]
            .iter()
            .position(|e| self.origin.get(&e.id) == Some(&before))
            .map(|i| cur + 1 + i);
        if let Some(i) = duplicate {
            let e = self.entries.remove(i);
            self.origin.remove(&e.id);
        }
        let entry = self.fresh(self.context[before].clone(), Some(before));
        let id = entry.id;
        self.entries.insert(cur, entry);
        self.current = Some(cur + 1);
        Some(id)
    }

    fn index_of(&self, id: QueueId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(n: u32) -> Track {
        Track {
            video_id: format!("vid{n:08}"),
            title: format!("titre {n}"),
            artist: "artiste".into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: None,
            thumbnail_url: None,
        }
    }

    fn tracks(range: std::ops::Range<u32>) -> Vec<Track> {
        range.map(track).collect()
    }

    fn queue() -> Queue {
        Queue::with_rng(fastrand::Rng::with_seed(7))
    }

    fn names(list: &[QueuedTrack]) -> Vec<String> {
        list.iter()
            .map(|e| e.track.title.trim_start_matches("titre ").to_owned())
            .collect()
    }

    fn visible(q: &Queue) -> Vec<String> {
        names(q.visible())
    }

    fn current(q: &Queue) -> Option<String> {
        q.current()
            .map(|e| e.track.title.trim_start_matches("titre ").to_owned())
    }

    #[test]
    fn shuffle_on_empty_queue_never_requeues_the_current_track() {
        // Le morceau en cours figure deux fois dans le contexte (réajouté).
        let mut q = queue();
        q.replace(tracks(0..1), 0);
        q.append(tracks(0..1));
        q.advance_auto();
        assert_eq!(visible(&q), ["0"]);
        q.set_shuffle(true);
        assert_eq!(
            visible(&q),
            ["0"],
            "rien ne revient derrière le morceau en cours"
        );
        // Suite vidée puis aléatoire : toujours rien.
        let mut q = queue();
        q.replace(tracks(0..3), 1);
        q.clear_upcoming();
        q.set_shuffle(true);
        assert_eq!(visible(&q), ["1"]);
    }

    #[test]
    fn starting_mid_playlist_plays_everything_else_after() {
        let mut q = queue();
        q.replace(tracks(0..5), 2);
        // Rien n'est « déjà joué » : la fin de la playlist, puis son début.
        assert_eq!(visible(&q), ["2", "3", "4", "0", "1"]);
        assert_eq!(q.previous(), None, "aucun historique au départ");
    }

    #[test]
    fn played_tracks_leave_the_queue_but_previous_comes_back() {
        let mut q = queue();
        q.replace(tracks(0..4), 0);
        q.advance_auto();
        q.advance_auto();
        assert_eq!(visible(&q), ["2", "3"]);
        let prev = q.previous().unwrap();
        q.set_current(prev);
        assert_eq!(current(&q).as_deref(), Some("1"));
        assert_eq!(visible(&q), ["1", "2", "3"]);
    }

    #[test]
    fn previous_without_history_goes_to_the_track_before() {
        let mut q = queue();
        q.replace(tracks(0..4), 2);
        let prev = q.prepare_previous().unwrap();
        q.set_current(prev);
        assert_eq!(visible(&q), ["1", "2", "3", "0"]);
    }

    #[test]
    fn jumping_ahead_skips_without_greying() {
        let mut q = queue();
        q.replace(tracks(0..6), 0);
        let target = q.visible()[3].id; // titre 3
        q.set_current(target);
        assert_eq!(visible(&q), ["3", "4", "5"]);
        // Les morceaux sautés reviennent si on reconstruit la suite.
        q.set_shuffle(false);
        assert_eq!(visible(&q), ["3", "4", "5", "0", "1", "2"]);
    }

    #[test]
    fn shuffle_on_and_off_rebuilds_the_whole_list() {
        let mut q = queue();
        q.replace(tracks(0..30), 25);
        q.advance_auto(); // 26
        q.set_shuffle(true);
        assert_eq!(
            q.upcoming_len(),
            29,
            "toute la playlist sauf le morceau en cours"
        );
        let order = visible(&q);
        let natural: Vec<String> = (27..30).chain(0..26).map(|n| n.to_string()).collect();
        assert_ne!(order[1..], natural[..]);
        q.set_shuffle(false);
        let mut expected = vec!["26".to_owned()];
        expected.extend(natural);
        assert_eq!(visible(&q), expected);
        // Réactiver : de nouveau toute la liste, pas seulement la fin.
        q.set_shuffle(true);
        assert_eq!(q.upcoming_len(), 29);
        assert_eq!(current(&q).as_deref(), Some("26"));
        assert!(q.previous().is_some(), "l'historique reste");
    }

    #[test]
    fn shuffled_play_queues_the_whole_list() {
        let mut q = queue();
        q.replace_shuffled(tracks(0..50));
        assert!(q.shuffle());
        assert_eq!(q.upcoming_len(), 49);
    }

    #[test]
    fn auto_advance_respects_repeat() {
        let mut q = queue();
        q.replace(tracks(0..2), 0);
        assert!(q.advance_auto().is_some());
        assert_eq!(q.advance_auto(), None, "fin de liste sans repeat");
        q.set_repeat(Repeat::All);
        assert!(q.advance_auto().is_some());
        assert_eq!(current(&q).as_deref(), Some("0"), "la liste repart");
        q.set_repeat(Repeat::One);
        q.advance_auto();
        assert_eq!(current(&q).as_deref(), Some("0"));
    }

    #[test]
    fn manual_next_ignores_repeat_one() {
        let mut q = queue();
        q.replace(tracks(0..2), 0);
        q.set_repeat(Repeat::One);
        let next = q.next_manual().unwrap();
        assert_eq!(q.get(next).unwrap().track.title, "titre 1");
    }

    #[test]
    fn insert_next_goes_right_after_current() {
        let mut q = queue();
        q.replace(tracks(0..3), 1);
        q.insert_next(vec![track(9)]);
        assert_eq!(visible(&q), ["1", "9", "2", "0"]);
        // Et il est dans la liste d'origine, juste après le courant.
        q.set_shuffle(false);
        assert_eq!(visible(&q), ["1", "9", "2", "0"]);
    }

    #[test]
    fn removed_tracks_do_not_come_back() {
        let mut q = queue();
        q.replace(tracks(0..4), 0);
        let two = q.visible()[2].id;
        assert_eq!(q.remove(two), Removed::Upcoming);
        q.set_shuffle(false);
        assert_eq!(visible(&q), ["0", "1", "3"]);
        let cur = q.current().unwrap().id;
        assert_eq!(q.remove(cur), Removed::Current);
        assert!(q.current().is_none());
        assert_eq!(names(q.upcoming()), ["1", "3"]);
        assert_eq!(q.remove(QueueId(999)), Removed::NotFound);
    }

    #[test]
    fn move_is_relative_to_the_visible_queue() {
        let mut q = queue();
        q.replace(tracks(0..4), 0);
        let three = q.visible()[3].id;
        assert!(q.move_to(three, 1));
        assert_eq!(visible(&q), ["0", "3", "1", "2"]);
        assert_eq!(current(&q).as_deref(), Some("0"));
    }

    #[test]
    fn clear_upcoming_keeps_current_and_does_not_refill() {
        let mut q = queue();
        q.replace(tracks(0..5), 1);
        q.clear_upcoming();
        assert_eq!(visible(&q), ["1"]);
        q.set_shuffle(true);
        assert_eq!(visible(&q), ["1"]);
    }

    #[test]
    fn radio_tracks_are_mixed_in_when_shuffled() {
        let mut q = queue();
        q.replace(tracks(0..4), 0);
        q.set_shuffle(true);
        let next = q.upcoming()[0].id;
        q.append_mixed(tracks(10..30));
        assert_eq!(q.upcoming_len(), 23);
        assert_eq!(
            q.upcoming()[0].id,
            next,
            "le prochain (préchargé) ne bouge pas"
        );
    }

    #[test]
    fn restore_keeps_history_and_order() {
        let mut q = queue();
        q.restore(tracks(0..5), 2, false);
        assert_eq!(current(&q).as_deref(), Some("2"));
        assert_eq!(visible(&q), ["2", "3", "4"]);
        assert!(q.previous().is_some());
    }

    #[test]
    fn repeat_cycles_like_youtube_music() {
        assert_eq!(Repeat::Off.cycle(), Repeat::All);
        assert_eq!(Repeat::All.cycle(), Repeat::One);
        assert_eq!(Repeat::One.cycle(), Repeat::Off);
    }
}
