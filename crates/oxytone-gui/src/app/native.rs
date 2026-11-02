//! Fenêtre native (Windows) : identifiant de la fenêtre pour le système.
//!
//! En clair : les boutons de la barre des tâches, l'overlay média et le plein
//! écran fait main ont besoin de la « poignée » Windows de la fenêtre.

use iced::window;

/// HWND sous Windows (exigé par l'overlay média) ; rien ailleurs.
pub(super) fn native_handle(window: &dyn window::Window) -> Option<usize> {
    let hwnd = window_hwnd(window)?;
    // Sur le fil de la fenêtre : boutons du mini-lecteur de la barre des tâches.
    #[cfg(windows)]
    oxytone_os::taskbar::install(hwnd);
    Some(hwnd)
}

/// HWND de la fenêtre (Windows), sans rien installer.
pub(super) fn window_hwnd(window: &dyn window::Window) -> Option<usize> {
    use raw_window_handle::RawWindowHandle;
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as usize),
        _ => None,
    }
}
