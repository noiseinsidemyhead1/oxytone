//! Plein écran fait main sous Windows (F11).
//!
//! En clair : la fenêtre de ytmr n'a pas de cadre système. Le plein écran de
//! winit, à la sortie, lui réappliquait un style mémorisé qui ne lui
//! correspond plus : à partir de la deuxième fois, Windows dessinait un vieux
//! cadre (barre de titre bleue façon Windows 7). Ici, on ne touche jamais au
//! style : on note la place de la fenêtre, on l'étend sur tout l'écran où
//! elle se trouve (Windows masque alors la barre des tâches), puis on la remet
//! exactement où elle était.

#![allow(unsafe_code)] // API Win32 (placement de fenêtre), sans équivalent sûr.

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, HWND_TOP, SWP_FRAMECHANGED, SWP_NOOWNERZORDER, SetWindowPlacement,
    SetWindowPos, WINDOWPLACEMENT,
};

/// Place de la fenêtre avant le plein écran (taille, position, agrandie ou non).
static SAVED: Mutex<Option<WINDOWPLACEMENT>> = Mutex::new(None);

/// Passe en plein écran (`true`) ou en sort. À appeler sur le fil de la fenêtre.
pub fn set(hwnd: usize, on: bool) {
    let window = HWND(hwnd as *mut _);
    let result = if on { enter(window) } else { leave(window) };
    if let Err(e) = result {
        tracing::warn!(error = %e, "plein écran");
    }
}

fn enter(window: HWND) -> windows::core::Result<()> {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..WINDOWPLACEMENT::default()
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    unsafe {
        GetWindowPlacement(window, &mut placement)?;
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err(windows::core::Error::from_thread());
        }
    }
    // Déjà en plein écran : on garde la place d'origine.
    if let Ok(mut saved) = SAVED.lock()
        && saved.is_none()
    {
        *saved = Some(placement);
    }
    let r = info.rcMonitor;
    unsafe {
        SetWindowPos(
            window,
            Some(HWND_TOP),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        )
    }
}

fn leave(window: HWND) -> windows::core::Result<()> {
    let saved = SAVED.lock().ok().and_then(|mut s| s.take());
    let Some(placement) = saved else {
        return Ok(());
    };
    unsafe {
        SetWindowPlacement(window, &placement)?;
        SetWindowPos(
            window,
            None,
            0,
            0,
            0,
            0,
            windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                | SWP_NOOWNERZORDER
                | SWP_FRAMECHANGED,
        )
    }
}
