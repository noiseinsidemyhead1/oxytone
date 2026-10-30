//! Paroles : chargement et suivi de la ligne chantée.
//!
//! En clair : quand le panneau des paroles est ouvert, ytmr les demande pour
//! le morceau en cours et fait glisser la liste jusqu'à la ligne chantée.

use super::*;

/// Hauteur de chaque ligne de paroles dans un panneau de `width` px : une
/// ligne longue passe sur plusieurs lignes de texte. Estimation volontairement
/// large (0,62 × la taille par caractère, gras) : un peu d'air plutôt qu'un
/// chevauchement. Sert à l'affichage et au défilement (même calcul).
pub fn lyric_heights(texts: impl Iterator<Item = usize>, width: f32) -> Vec<f32> {
    let per_line = ((width / (LYRIC_SIZE * 0.62)).floor() as usize).max(8);
    texts
        .map(|chars| {
            let rows = chars.max(1).div_ceil(per_line).min(5) as f32;
            rows * LYRIC_TEXT_H + LYRIC_GAP
        })
        .collect()
}

/// Largeur utile du texte dans le panneau des paroles.
pub fn lyrics_text_width(panel: f32) -> f32 {
    panel - 40.0
}

impl App {
    pub(crate) fn lyrics_shown(&self) -> bool {
        self.show_queue && self.right_panel == RightPanel::Lyrics
    }

    /// Fait glisser les paroles jusqu'à la ligne chantée (quand elle change).
    pub(super) fn follow_lyrics(&mut self) {
        if !self.lyrics_shown() {
            return;
        }
        let Some(entry) = &self.state.current else {
            return;
        };
        let Some(Load::Ready(Some(lyrics))) = self.lyrics.get(&entry.track.video_id) else {
            return;
        };
        let line = lyrics.line_at(self.state.position + Duration::from_millis(200));
        let manual = self
            .lyrics_manual_until
            .is_some_and(|t| std::time::Instant::now() < t);
        if line == self.lyrics_line || manual {
            return;
        }
        self.lyrics_line = line;
        // La ligne chantée (son milieu) se place aux deux cinquièmes du panneau.
        let ytmr_catalog::lyrics::LyricsBody::Synced(lines) = &lyrics.body else {
            return;
        };
        let heights = lyric_heights(
            lines.iter().map(|l| l.text.chars().count()),
            lyrics_text_width(self.queue_width),
        );
        let index = line.unwrap_or(0).min(heights.len().saturating_sub(1));
        let above: f32 = heights[..index].iter().sum();
        let middle = heights.get(index).copied().unwrap_or(0.0) / 2.0;
        let target = LYRICS_TOP + above + middle - self.lyrics_view.height * 0.4;
        self.scrolling.glide_to(LYRICS_SCROLL, target);
    }

    /// Paroles du morceau en cours, quand la grande vue est ouverte.
    pub(super) fn want_lyrics(&mut self) -> Task<Message> {
        if !self.lyrics_shown() {
            return Task::none();
        }
        let Some(track) = self.state.current.as_ref().map(|e| e.track.clone()) else {
            return Task::none();
        };
        if self.lyrics.contains_key(&track.video_id) {
            return Task::none();
        }
        let Some(catalog) = self.catalog() else {
            return Task::none();
        };
        if self.lyrics.len() >= LYRICS_KEPT {
            self.lyrics.clear();
        }
        self.lyrics.insert(track.video_id.clone(), Load::Loading);
        Task::perform(
            async move {
                let result = catalog.lyrics(&track).await.map_err(|e| e.to_string());
                (track.video_id, result)
            },
            |(id, r)| Message::LyricsLoaded(id, r),
        )
    }
}
