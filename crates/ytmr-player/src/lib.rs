//! Lecture audio derrière un trait `Player`, implémenté par mpv piloté en JSON IPC.
//!
//! En clair : la télécommande de mpv. mpv (aidé de yt-dlp) fait le vrai travail
//! de lecture ; on ne touche jamais à l'extraction des flux YouTube.
//! (Phase 1.)
