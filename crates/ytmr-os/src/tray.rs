//! Icône de ytmr dans la zone de notification de Windows (à côté de l'horloge).
//!
//! En clair : quand le réglage est actif, une petite icône reste près de
//! l'horloge ; fermer la fenêtre la cache seulement (la musique continue).
//! Clic sur l'icône : la fenêtre revient. Clic droit : lecture / pause,
//! suivant, précédent, afficher, quitter.
//!
//! Les messages de l'icône arrivent à la fenêtre principale : le
//! sous-classement de [`crate::taskbar`] les fait suivre à [`on_message`].

#![allow(unsafe_code)] // API Win32 (Shell_NotifyIcon, menus), sans équivalent sûr.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, ICONINFO,
    IsIconic, MF_SEPARATOR, MF_STRING, PostMessageW, SW_HIDE, SW_RESTORE, SW_SHOW,
    SetForegroundWindow, ShowWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
    WM_CLOSE, WM_LBUTTONUP, WM_RBUTTONUP,
};
use windows::core::PCWSTR;

/// Message de l'icône vers la fenêtre (`lparam` = évènement souris).
pub(crate) const WM_YTMR_TRAY: u32 = WM_APP + 0x40;
const ICON_ID: u32 = 1;
const ICON_PX: usize = 32;

const MENU_TOGGLE: u32 = 1;
const MENU_NEXT: u32 = 2;
const MENU_PREV: u32 = 3;
const MENU_SHOW: u32 = 4;
const MENU_QUIT: u32 = 5;

/// Réglage actif (l'icône est posée dès que la fenêtre est connue).
static ENABLED: AtomicBool = AtomicBool::new(false);
/// L'icône est actuellement dans la zone de notification.
static SHOWN: AtomicBool = AtomicBool::new(false);
/// « Quitter » choisi dans le menu : la fermeture qui suit est la vraie.
static QUIT: AtomicBool = AtomicBool::new(false);

/// Active ou retire l'icône (réglage de l'utilisateur).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    match crate::taskbar::window() {
        Some(window) if on => add(window),
        Some(window) => remove(window),
        None => {}
    }
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// « Quitter » a été choisi dans le menu de l'icône.
pub fn quit_requested() -> bool {
    QUIT.load(Ordering::Relaxed)
}

/// Deuxième lancement : autorise l'instance déjà ouverte à passer au premier
/// plan (Windows le refuse sinon à un programme en arrière-plan).
pub fn allow_foreground() {
    use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

/// Ramène la fenêtre (cachée, réduite ou derrière les autres).
pub fn show() {
    if let Some(window) = crate::taskbar::window() {
        show_window(window);
    }
}

/// Cache la fenêtre (fermeture avec l'icône active : la musique continue).
pub fn hide_window() {
    if let Some(window) = crate::taskbar::window() {
        unsafe {
            let _ = ShowWindow(window, SW_HIDE);
        }
    }
}

/// Retire l'icône à l'arrêt de l'appli (sinon elle reste jusqu'au survol).
pub fn shutdown() {
    if let Some(window) = crate::taskbar::window() {
        remove(window);
    }
}

/// Fenêtre connue (ou recréation de la barre des tâches) : l'icône est (re)posée.
pub(crate) fn restore(window: HWND) {
    if enabled() {
        SHOWN.store(false, Ordering::Relaxed);
        add(window);
    }
}

fn data(window: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        ..Default::default()
    }
}

fn add(window: HWND) {
    if SHOWN.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(icon) = app_icon() else {
        SHOWN.store(false, Ordering::Relaxed);
        return;
    };
    let mut d = data(window);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    d.uCallbackMessage = WM_YTMR_TRAY;
    d.hIcon = icon;
    for (slot, unit) in d.szTip.iter_mut().zip("ytmr".encode_utf16()) {
        *slot = unit;
    }
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &d) }.as_bool() {
        tracing::debug!("icône de la zone de notification refusée");
        SHOWN.store(false, Ordering::Relaxed);
    }
}

fn remove(window: HWND) {
    if SHOWN.swap(false, Ordering::Relaxed) {
        let d = data(window);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }
}

fn show_window(window: HWND) {
    unsafe {
        let _ = ShowWindow(
            window,
            if IsIconic(window).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        let _ = SetForegroundWindow(window);
    }
}

/// Messages de l'icône (appelé par le sous-classement de la fenêtre).
pub(crate) fn on_message(window: HWND, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> bool {
    if msg != WM_YTMR_TRAY {
        return false;
    }
    match (lparam.0 & 0xffff) as u32 {
        WM_LBUTTONUP => show_window(window),
        WM_RBUTTONUP => menu(window),
        _ => {}
    }
    true
}

fn menu(window: HWND) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain([0]).collect() };
    let items = [
        (MENU_TOGGLE, "Lecture / pause"),
        (MENU_NEXT, "Suivant"),
        (MENU_PREV, "Précédent"),
        (0, ""),
        (MENU_SHOW, "Afficher ytmr"),
        (MENU_QUIT, "Quitter"),
    ];
    // Les textes doivent vivre jusqu'à la fin du menu.
    let labels: Vec<Vec<u16>> = items.iter().map(|(_, s)| wide(s)).collect();
    for ((id, _), label) in items.iter().zip(&labels) {
        let result = unsafe {
            if *id == 0 {
                AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())
            } else {
                AppendMenuW(menu, MF_STRING, *id as usize, PCWSTR(label.as_ptr()))
            }
        };
        if result.is_err() {
            tracing::debug!("menu de l'icône incomplet");
        }
    }
    let mut point = POINT::default();
    let chosen = unsafe {
        let _ = GetCursorPos(&mut point);
        // Sans cela, le menu ne se ferme pas quand on clique ailleurs.
        let _ = SetForegroundWindow(window);
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            None,
            window,
            None,
        )
    };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    match chosen.0 as u32 {
        MENU_TOGGLE => crate::taskbar::send(ytmr_core::Command::TogglePause),
        MENU_NEXT => crate::taskbar::send(ytmr_core::Command::Next),
        MENU_PREV => crate::taskbar::send(ytmr_core::Command::Previous),
        MENU_SHOW => show_window(window),
        MENU_QUIT => {
            QUIT.store(true, Ordering::Relaxed);
            // La fenêtre se ferme comme d'habitude (mpv arrêté proprement).
            unsafe {
                let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        _ => {}
    }
}

// ----- icône dessinée en code : le disque de ytmr (dégradé rose-violet percé) -----

/// Pixels BGRA prémultipliés (lissés par 4 × 4 échantillons), même dessin
/// que l'icône de l'appli (`packaging/make_icon.py`).
fn pixels() -> Vec<u8> {
    let mut out = vec![0u8; ICON_PX * ICON_PX * 4];
    for py in 0..ICON_PX {
        // Dégradé vertical : rose en haut, violet en bas (RVB).
        let t = py as f32 / ICON_PX as f32;
        let rgb = [255.0 - 60.0 * t, 60.0 + 10.0 * t, 110.0 + 110.0 * t];
        for px in 0..ICON_PX {
            let mut covered = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / ICON_PX as f32 - 0.5;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / ICON_PX as f32 - 0.5;
                    let d = (x * x + y * y).sqrt() / 0.5;
                    covered += u32::from((0.2..=1.0).contains(&d));
                }
            }
            let alpha = covered as f32 / 16.0;
            let i = (py * ICON_PX + px) * 4;
            // BGRA, prémultiplié par la couverture.
            out[i] = (rgb[2] * alpha).round() as u8;
            out[i + 1] = (rgb[1] * alpha).round() as u8;
            out[i + 2] = (rgb[0] * alpha).round() as u8;
            out[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    out
}

fn app_icon() -> Option<HICON> {
    static ICON: OnceLock<Option<isize>> = OnceLock::new();
    let raw = ICON.get_or_init(|| {
        let bits = pixels();
        let side = ICON_PX as i32;
        unsafe {
            let color = CreateBitmap(side, side, 1, 32, Some(bits.as_ptr().cast()));
            // Masque 1 bit : 4 octets par ligne de 32 px.
            let mask_bits = [0u8; 4 * ICON_PX];
            let mask = CreateBitmap(side, side, 1, 1, Some(mask_bits.as_ptr().cast()));
            let info = ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            };
            let icon = CreateIconIndirect(&info);
            let _ = DeleteObject(HGDIOBJ(color.0));
            let _ = DeleteObject(HGDIOBJ(mask.0));
            icon.ok().map(|i| i.0 as isize)
        }
    });
    raw.map(|h| HICON(h as *mut _))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_pierced_disc() {
        let px = pixels();
        let at = |x: usize, y: usize| &px[(y * ICON_PX + x) * 4..(y * ICON_PX + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "coin transparent");
        assert_eq!(at(16, 4)[3], 255, "disque plein");
        assert!(at(16, 4)[2] > at(16, 4)[0], "rose en haut");
        assert_eq!(at(16, 16)[3], 0, "trou au centre");
    }
}
