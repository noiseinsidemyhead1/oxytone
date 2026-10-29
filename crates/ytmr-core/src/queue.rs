//! File d'attente : ordre de lecture, shuffle et repeat.
//!
//! En clair : la liste des morceaux à jouer et un curseur sur celui en cours.
//! Chaque entrée reçoit un identifiant unique ([`QueueId`]), pour qu'on puisse
//! retirer ou déplacer « ce morceau-là » même si le même titre apparaît deux fois.
//!
//! Le shuffle mélange réellement la suite de la liste (comme YouTube Music) et
//! mémorise l'ordre d'origine pour le restaurer quand on le désactive.
//! Ce module ne fait aucune entrée/sortie : il est entièrement testé à part.

use crate::Track;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueueId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedTrack {
    pub id: QueueId,
    pub track: Track,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off,
    /// Reprend au début après le dernier morceau.
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

pub struct Queue {
    entries: Vec<QueuedTrack>,
    current: Option<usize>,
    /// Ordre d'origine, mémorisé tant que le shuffle est actif.
    unshuffled: Option<Vec<QueueId>>,
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
            unshuffled: None,
            repeat: Repeat::Off,
            next_id: 1,
            rng,
        }
    }

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

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    pub fn shuffle(&self) -> bool {
        self.unshuffled.is_some()
    }

    pub fn contains_video(&self, video_id: &str) -> bool {
        self.entries.iter().any(|e| e.track.video_id == video_id)
    }

    /// Morceaux restant après le courant (tous si rien n'est en cours).
    pub fn upcoming_len(&self) -> usize {
        match self.current {
            Some(i) => self.entries.len() - i - 1,
            None => self.entries.len(),
        }
    }

    /// Vide tout et met `tracks` à la place ; `start` devient le morceau courant.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) -> Option<QueueId> {
        self.entries.clear();
        self.current = None;
        let shuffle = self.shuffle();
        self.unshuffled = None;
        self.append(tracks);
        if self.entries.is_empty() {
            return None;
        }
        self.current = Some(start.min(self.entries.len() - 1));
        if shuffle {
            self.set_shuffle(true);
        }
        self.current().map(|e| e.id)
    }

    /// Ajoute à la fin de la file.
    pub fn append(&mut self, tracks: Vec<Track>) -> Vec<QueueId> {
        let at = self.entries.len();
        self.insert_at(at, tracks)
    }

    /// Insère juste après le morceau courant (« lire ensuite »).
    pub fn insert_next(&mut self, tracks: Vec<Track>) -> Vec<QueueId> {
        let at = self.current.map_or(0, |i| i + 1);
        self.insert_at(at, tracks)
    }

    fn insert_at(&mut self, at: usize, tracks: Vec<Track>) -> Vec<QueueId> {
        let new: Vec<QueuedTrack> = tracks
            .into_iter()
            .map(|track| {
                let id = QueueId(self.next_id);
                self.next_id += 1;
                QueuedTrack { id, track }
            })
            .collect();
        let ids: Vec<QueueId> = new.iter().map(|e| e.id).collect();
        if let Some(order) = &mut self.unshuffled {
            order.extend(&ids);
        }
        if let Some(cur) = self.current
            && at <= cur
        {
            self.current = Some(cur + new.len());
        }
        self.entries.splice(at..at, new);
        ids
    }

    pub fn remove(&mut self, id: QueueId) -> Removed {
        let Some(index) = self.index_of(id) else {
            return Removed::NotFound;
        };
        self.entries.remove(index);
        if let Some(order) = &mut self.unshuffled {
            order.retain(|&o| o != id);
        }
        match self.current {
            Some(cur) if cur == index => {
                self.current = None;
                Removed::Current
            }
            Some(cur) if index < cur => {
                self.current = Some(cur - 1);
                Removed::Upcoming
            }
            _ => Removed::Upcoming,
        }
    }

    /// Déplace une entrée à la position `to` (bornée à la longueur de la file).
    pub fn move_to(&mut self, id: QueueId, to: usize) -> bool {
        let Some(from) = self.index_of(id) else {
            return false;
        };
        let current_id = self.current().map(|e| e.id);
        let entry = self.entries.remove(from);
        let to = to.min(self.entries.len());
        self.entries.insert(to, entry);
        self.current = current_id.and_then(|cid| self.index_of(cid));
        true
    }

    /// Retire tout ce qui suit le morceau courant.
    pub fn clear_upcoming(&mut self) {
        let keep = self.current.map_or(0, |i| i + 1);
        let removed: Vec<QueueId> = self.entries.drain(keep..).map(|e| e.id).collect();
        if let Some(order) = &mut self.unshuffled {
            order.retain(|o| !removed.contains(o));
        }
    }

    pub fn set_current(&mut self, id: QueueId) -> bool {
        match self.index_of(id) {
            Some(i) => {
                self.current = Some(i);
                true
            }
            None => false,
        }
    }

    /// Ce qui sera joué à la fin naturelle du morceau courant (sans rien changer).
    pub fn peek_auto_next(&self) -> Option<&QueuedTrack> {
        self.auto_next_index().map(|i| &self.entries[i])
    }

    fn auto_next_index(&self) -> Option<usize> {
        let cur = self.current?;
        if self.repeat == Repeat::One {
            return Some(cur);
        }
        self.following(cur)
    }

    /// Entrée après `index` ; ne boucle qu'en Repeat::All.
    fn following(&self, index: usize) -> Option<usize> {
        if index + 1 < self.entries.len() {
            Some(index + 1)
        } else if self.repeat == Repeat::All && !self.entries.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    /// Fin naturelle du morceau : avance le curseur et rend le nouveau courant.
    pub fn advance_auto(&mut self) -> Option<QueueId> {
        let next = self.auto_next_index()?;
        self.current = Some(next);
        Some(self.entries[next].id)
    }

    /// Bouton « suivant » : ignore Repeat::One, boucle seulement en Repeat::All.
    pub fn next_manual(&self) -> Option<QueueId> {
        let next = match self.current {
            Some(cur) => self.following(cur)?,
            None => 0,
        };
        self.entries.get(next).map(|e| e.id)
    }

    /// Bouton « précédent » (le core décide avant s'il faut plutôt revenir au début).
    pub fn previous(&self) -> Option<QueueId> {
        let cur = self.current?;
        let prev = match cur.checked_sub(1) {
            Some(p) => p,
            None if self.repeat == Repeat::All => self.entries.len().checked_sub(1)?,
            None => return None,
        };
        self.entries.get(prev).map(|e| e.id)
    }

    /// Active : mélange tout ce qui suit le morceau courant. Désactive : remet
    /// l'ordre d'origine (les ajouts faits entre-temps restent à la fin).
    pub fn set_shuffle(&mut self, on: bool) {
        if on {
            if self.unshuffled.is_none() {
                self.unshuffled = Some(self.entries.iter().map(|e| e.id).collect());
            }
            let start = self.current.map_or(0, |i| i + 1);
            self.rng.shuffle(&mut self.entries[start..]);
        } else if let Some(order) = self.unshuffled.take() {
            let current_id = self.current().map(|e| e.id);
            let rank = |id: QueueId| order.iter().position(|&o| o == id).unwrap_or(usize::MAX);
            self.entries.sort_by_key(|e| rank(e.id));
            self.current = current_id.and_then(|cid| self.index_of(cid));
        }
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
            album: None,
            album_id: None,
            duration: None,
            thumbnail_url: None,
        }
    }

    fn tracks(range: std::ops::Range<u32>) -> Vec<Track> {
        range.map(track).collect()
    }

    fn titles(q: &Queue) -> Vec<String> {
        q.entries().iter().map(|e| e.track.title.clone()).collect()
    }

    fn current_title(q: &Queue) -> Option<String> {
        q.current().map(|e| e.track.title.clone())
    }

    fn queue() -> Queue {
        Queue::with_rng(fastrand::Rng::with_seed(42))
    }

    #[test]
    fn replace_sets_current_and_clamps_start() {
        let mut q = queue();
        q.replace(tracks(0..3), 1);
        assert_eq!(current_title(&q).as_deref(), Some("titre 1"));
        q.replace(tracks(0..3), 99);
        assert_eq!(current_title(&q).as_deref(), Some("titre 2"));
        assert_eq!(q.replace(vec![], 0), None);
        assert!(q.current().is_none());
    }

    #[test]
    fn auto_advance_respects_repeat() {
        let mut q = queue();
        q.replace(tracks(0..2), 0);
        assert!(q.advance_auto().is_some());
        assert_eq!(current_title(&q).as_deref(), Some("titre 1"));
        assert_eq!(q.advance_auto(), None, "fin de file sans repeat");

        q.set_repeat(Repeat::All);
        q.advance_auto();
        assert_eq!(current_title(&q).as_deref(), Some("titre 0"));

        q.set_repeat(Repeat::One);
        q.advance_auto();
        assert_eq!(current_title(&q).as_deref(), Some("titre 0"));
    }

    #[test]
    fn manual_next_ignores_repeat_one() {
        let mut q = queue();
        q.replace(tracks(0..2), 0);
        q.set_repeat(Repeat::One);
        let next = q.next_manual().unwrap();
        assert_eq!(q.get(next).unwrap().track.title, "titre 1");

        q.set_current(next);
        assert_eq!(q.next_manual(), None, "pas de boucle en Repeat::One");
        q.set_repeat(Repeat::All);
        assert_eq!(
            q.get(q.next_manual().unwrap()).unwrap().track.title,
            "titre 0"
        );
    }

    #[test]
    fn previous_wraps_only_with_repeat_all() {
        let mut q = queue();
        q.replace(tracks(0..3), 0);
        assert_eq!(q.previous(), None);
        q.set_repeat(Repeat::All);
        assert_eq!(q.get(q.previous().unwrap()).unwrap().track.title, "titre 2");
    }

    #[test]
    fn insert_next_goes_right_after_current() {
        let mut q = queue();
        q.replace(tracks(0..3), 1);
        q.insert_next(vec![track(9)]);
        assert_eq!(titles(&q), ["titre 0", "titre 1", "titre 9", "titre 2"]);
        assert_eq!(current_title(&q).as_deref(), Some("titre 1"));
        assert_eq!(q.peek_auto_next().unwrap().track.title, "titre 9");
    }

    #[test]
    fn remove_keeps_cursor_on_the_same_track() {
        let mut q = queue();
        let first = q.append(tracks(0..4));
        q.set_current(first[2]);

        assert_eq!(q.remove(first[0]), Removed::Upcoming);
        assert_eq!(current_title(&q).as_deref(), Some("titre 2"));
        assert_eq!(q.remove(first[2]), Removed::Current);
        assert!(q.current().is_none());
        assert_eq!(q.remove(QueueId(999)), Removed::NotFound);
    }

    #[test]
    fn move_keeps_cursor_on_the_same_track() {
        let mut q = queue();
        let ids = q.append(tracks(0..4));
        q.set_current(ids[1]);
        assert!(q.move_to(ids[3], 0));
        assert_eq!(titles(&q), ["titre 3", "titre 0", "titre 1", "titre 2"]);
        assert_eq!(current_title(&q).as_deref(), Some("titre 1"));
        assert!(q.move_to(ids[0], 99));
        assert_eq!(titles(&q).last().map(String::as_str), Some("titre 0"));
    }

    #[test]
    fn clear_upcoming_keeps_history_and_current() {
        let mut q = queue();
        q.replace(tracks(0..5), 2);
        q.clear_upcoming();
        assert_eq!(titles(&q), ["titre 0", "titre 1", "titre 2"]);
        assert_eq!(q.upcoming_len(), 0);
    }

    #[test]
    fn shuffle_mixes_only_upcoming_and_restores_order() {
        let mut q = queue();
        q.replace(tracks(0..20), 5);
        let before = titles(&q);

        q.set_shuffle(true);
        let shuffled = titles(&q);
        assert_eq!(shuffled[..6], before[..6], "historique et courant intacts");
        assert_ne!(shuffled[6..], before[6..]);
        assert_eq!(current_title(&q).as_deref(), Some("titre 5"));

        // Un ajout pendant le shuffle reste à la fin une fois désactivé.
        q.append(vec![track(99)]);
        q.advance_auto();
        let playing = current_title(&q);
        q.set_shuffle(false);
        let mut expected = before.clone();
        expected.push("titre 99".into());
        assert_eq!(titles(&q), expected);
        assert_eq!(
            current_title(&q),
            playing,
            "le morceau en cours ne change pas"
        );
    }

    #[test]
    fn replace_keeps_shuffle_mode() {
        let mut q = queue();
        q.set_shuffle(true);
        q.replace(tracks(0..20), 0);
        assert!(q.shuffle());
        assert_eq!(current_title(&q).as_deref(), Some("titre 0"));
        assert_ne!(
            titles(&q),
            titles(&{
                let mut plain = queue();
                plain.replace(tracks(0..20), 0);
                plain
            })
        );
    }

    #[test]
    fn repeat_cycles_like_youtube_music() {
        assert_eq!(Repeat::Off.cycle(), Repeat::All);
        assert_eq!(Repeat::All.cycle(), Repeat::One);
        assert_eq!(Repeat::One.cycle(), Repeat::Off);
    }
}
