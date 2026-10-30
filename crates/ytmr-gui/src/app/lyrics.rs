//! Paroles : chargement et suivi de la ligne chantée.
//!
//! En clair : quand le panneau des paroles est ouvert, ytmr les demande pour
//! le morceau en cours et fait glisser la liste jusqu'à la ligne chantée.

use super::*;

/// Hauteur de chaque ligne de paroles dans un panneau de `width` px : une
/// ligne longue passe sur plusieurs lignes de texte. Estimation volontairement
/// large (0,62 × la taille par caractère, gras) : un peu d'air plutôt qu'un
/// chevauchement. Sert à l'affichage et au défilement (même calcul).
pub fn lyric_heights(texts: impl Iterator<Item = usize>, width: f32, size: f32) -> Vec<f32> {
    let per_line = ((width / (size * 0.62)).floor() as usize).max(8);
    // Hauteur d'une ligne de texte et écart entre deux lignes, à l'échelle.
    let (text_h, gap) = (size * 1.35, size * 0.8);
    texts
        .map(|chars| {
            let rows = chars.max(1).div_ceil(per_line).min(5) as f32;
            rows * text_h + gap
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

    /// Le morceau en cours a des paroles synchronisées (affichées en mode focus).
    pub(crate) fn np_synced(&self) -> bool {
        self.state.current.as_ref().is_some_and(|e| {
            matches!(
                self.lyrics.get(&e.track.video_id),
                Some(Load::Ready(Some(l))) if matches!(l.body, ytmr_catalog::lyrics::LyricsBody::Synced(_))
            )
        })
    }

    /// À l'ouverture du mode focus : la mise en page est tout de suite la bonne.
    pub(crate) fn np_split_snap(&mut self) {
        let target = if self.np_synced() { 1.0 } else { 0.0 };
        self.np_split = target;
        self.np_split_target = target;
        self.np_split_anim = None;
    }

    /// Largeur du texte des paroles dans la vue « En cours ».
    pub(crate) fn np_lyrics_text_width(&self) -> f32 {
        (self.window_width - 2.0 * NP_PAD - self.np_cover() - NP_GAP - 4.0)
            .clamp(200.0, NP_LYRICS_MAX)
    }

    /// Côté de la pochette de la vue « En cours » : suit la hauteur de la fenêtre.
    pub(crate) fn np_cover(&self) -> f32 {
        (self.window_size.height * 0.52).clamp(280.0, 620.0)
    }

    /// Où les paroles sont affichées : (défilement, largeur du texte, hauteur visible).
    fn lyrics_surfaces(&self) -> Vec<(&'static str, f32, f32, f32)> {
        let mut out = Vec::new();
        if self.lyrics_shown() {
            out.push((
                LYRICS_SCROLL,
                lyrics_text_width(self.queue_width),
                self.lyrics_view.height,
                LYRIC_SIZE,
            ));
        }
        if self.route == Route::NowPlaying {
            out.push((
                NP_LYRICS_SCROLL,
                self.np_lyrics_text_width(),
                self.np_lyrics_view.height,
                NP_LYRIC_SIZE,
            ));
        }
        out
    }

    /// Fait glisser les paroles jusqu'à la ligne chantée (quand elle change),
    /// dans le panneau de droite et dans la vue « En cours ».
    pub(super) fn follow_lyrics(&mut self) {
        let surfaces = self.lyrics_surfaces();
        if surfaces.is_empty() {
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
        let lengths: Vec<usize> = lines.iter().map(|l| l.text.chars().count()).collect();
        for (scroll, width, view_height, size) in surfaces {
            let heights = lyric_heights(lengths.iter().copied(), width, size);
            let index = line.unwrap_or(0).min(heights.len().saturating_sub(1));
            let above: f32 = heights[..index].iter().sum();
            let middle = heights.get(index).copied().unwrap_or(0.0) / 2.0;
            let target = LYRICS_TOP + above + middle - view_height * 0.4;
            self.scrolling.glide_to(scroll, target);
        }
    }

    /// Paroles du morceau en cours, quand le panneau ou la vue « En cours » les montre.
    pub(super) fn want_lyrics(&mut self) -> Task<Message> {
        if self.lyrics_surfaces().is_empty() {
            return Task::none();
        }
        let Some(track) = self.state.current.as_ref().map(|e| e.track.clone()) else {
            return Task::none();
        };
        if self.lyrics.contains_key(&track.video_id) {
            return Task::none();
        }
        let Some(catalog) = self.catalog() else {
            if self.lib.offline_mode {
                self.lyrics.insert(
                    track.video_id,
                    Load::Failed("indisponibles hors ligne".into()),
                );
            }
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
