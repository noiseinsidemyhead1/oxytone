//! Mini-lecteur de la barre des tâches Windows (boutons sous l'aperçu).
//!
//! En clair : au survol de l'icône de ytmr dans la barre des tâches, Windows
//! montre un aperçu de la fenêtre ; on y ajoute trois boutons (précédent,
//! lecture/pause, suivant), comme les lecteurs de musique classiques. Windows
//! signale les clics à la fenêtre par un message : on s'intercale devant la
//! procédure de fenêtre de winit (sous-classement) pour les attraper.
//!
//! Tout ce qui touche à la fenêtre se fait sur son fil (celui de l'interface) :
//! le fil de `ytmr-os` ne fait que lui poster un message quand l'état change.

#![allow(unsafe_code)] // API Win32 (COM, sous-classement de fenêtre), sans équivalent sûr.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use tokio::runtime::Handle;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList3, SetWindowSubclass, THB_FLAGS, THB_ICON, THB_TOOLTIP,
    THBF_ENABLED, THBN_CLICKED, THUMBBUTTON, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, HICON, ICONINFO, PostMessageW, RegisterWindowMessageW, WM_APP, WM_COMMAND,
};
use windows::core::w;
use ytmr_core::{Command, CoreHandle};

const ID_PREV: u32 = 1;
const ID_TOGGLE: u32 = 2;
const ID_NEXT: u32 = 3;
/// Message interne : `wparam` = 1 si la lecture est en cours.
const WM_YTMR_PLAYING: u32 = WM_APP + 0x31;
const SUBCLASS_ID: usize = 0x7974_6d72; // « ytmr »
const ICON_PX: usize = 20;

/// Moteur à qui envoyer les clics (connu une fois le backend démarré).
static CORE: OnceLock<(CoreHandle, Handle)> = OnceLock::new();
/// Fenêtre équipée des boutons (0 = aucune).
static WINDOW: AtomicIsize = AtomicIsize::new(0);

struct Bar {
    taskbar: ITaskbarList3,
    icons: Icons,
    playing: bool,
}

struct Icons {
    prev: HICON,
    play: HICON,
    pause: HICON,
    next: HICON,
}

thread_local! {
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

/// À appeler sur le fil de la fenêtre : pose les boutons et le sous-classement.
pub fn install(hwnd: usize) {
    let window = HWND(hwnd as *mut _);
    let result = BAR.with(|bar| -> windows::core::Result<()> {
        // COM est déjà initialisé sur ce fil par winit (glisser-déposer).
        let taskbar: ITaskbarList3 =
            unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)? };
        unsafe { taskbar.HrInit()? };
        let icons = Icons {
            prev: icon(Glyph::Prev)?,
            play: icon(Glyph::Play)?,
            pause: icon(Glyph::Pause)?,
            next: icon(Glyph::Next)?,
        };
        *bar.borrow_mut() = Some(Bar {
            taskbar,
            icons,
            playing: false,
        });
        Ok(())
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, "boutons de la barre des tâches indisponibles");
        return;
    }
    unsafe {
        let _ = SetWindowSubclass(window, Some(subclass), SUBCLASS_ID, 0);
    }
    WINDOW.store(hwnd as isize, Ordering::Relaxed);
    add_buttons(window);
    crate::tray::restore(window);
}

/// Fenêtre principale, une fois connue.
pub(crate) fn window() -> Option<HWND> {
    match WINDOW.load(Ordering::Relaxed) {
        0 => None,
        hwnd => Some(HWND(hwnd as *mut _)),
    }
}

/// Branche les clics sur le moteur (appelé par [`crate::start`]).
pub(crate) fn connect(core: CoreHandle, runtime: Handle) {
    let _ = CORE.set((core, runtime));
}

/// Depuis n'importe quel fil : le bouton du milieu montre lecture ou pause.
pub(crate) fn set_playing(playing: bool) {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut _)),
            WM_YTMR_PLAYING,
            WPARAM(usize::from(playing)),
            LPARAM(0),
        );
    }
}

fn tip(text: &str) -> [u16; 260] {
    let mut out = [0u16; 260];
    for (slot, unit) in out.iter_mut().zip(text.encode_utf16().take(259)) {
        *slot = unit;
    }
    out
}

fn buttons(bar: &Bar) -> [THUMBBUTTON; 3] {
    let button = |id: u32, icon: HICON, text: &str| THUMBBUTTON {
        dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        iId: id,
        hIcon: icon,
        szTip: tip(text),
        dwFlags: THBF_ENABLED,
        ..THUMBBUTTON::default()
    };
    let (toggle, label) = if bar.playing {
        (bar.icons.pause, "Pause")
    } else {
        (bar.icons.play, "Lecture")
    };
    [
        button(ID_PREV, bar.icons.prev, "Précédent"),
        button(ID_TOGGLE, toggle, label),
        button(ID_NEXT, bar.icons.next, "Suivant"),
    ]
}

fn add_buttons(window: HWND) {
    BAR.with(|bar| {
        if let Some(bar) = bar.borrow().as_ref()
            && let Err(e) = unsafe { bar.taskbar.ThumbBarAddButtons(window, &buttons(bar)) }
        {
            tracing::debug!(error = %e, "boutons de la barre des tâches");
        }
    });
}

fn update_buttons(window: HWND, playing: bool) {
    BAR.with(|bar| {
        if let Some(bar) = bar.borrow_mut().as_mut() {
            if bar.playing == playing {
                return;
            }
            bar.playing = playing;
            let _ = unsafe { bar.taskbar.ThumbBarUpdateButtons(window, &buttons(bar)) };
        }
    });
}

fn on_click(id: u32) {
    let command = match id {
        ID_PREV => Command::Previous,
        ID_TOGGLE => Command::TogglePause,
        ID_NEXT => Command::Next,
        _ => return,
    };
    send(command);
}

/// Envoie une commande au moteur (boutons de la barre, menu de l'icône).
pub(crate) fn send(command: Command) {
    let Some((core, runtime)) = CORE.get() else {
        return;
    };
    let core = core.clone();
    runtime.spawn(async move {
        if let Err(e) = core.send(command).await {
            tracing::debug!(error = %e, "commande de la barre des tâches");
        }
    });
}

/// Message envoyé par Explorer quand le bouton de la barre des tâches est
/// (re)créé (redémarrage d'Explorer) : les boutons sont à reposer.
fn taskbar_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) })
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_COMMAND && ((wparam.0 >> 16) & 0xffff) as u32 == THBN_CLICKED {
        on_click((wparam.0 & 0xffff) as u32);
        return LRESULT(0);
    }
    if msg == WM_YTMR_PLAYING {
        update_buttons(hwnd, wparam.0 != 0);
        return LRESULT(0);
    }
    if crate::tray::on_message(hwnd, msg, wparam, lparam) {
        return LRESULT(0);
    }
    if msg == taskbar_created() {
        add_buttons(hwnd);
        crate::tray::restore(hwnd);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

// ----- icônes dessinées à la main (blanc sur transparent) -----

#[derive(Clone, Copy)]
enum Glyph {
    Prev,
    Play,
    Pause,
    Next,
}

/// Le point (x, y), dans un carré 0..1, est-il dans le dessin ?
fn inside(glyph: Glyph, x: f32, y: f32) -> bool {
    let tri_right = |x0: f32, x1: f32| {
        // Triangle pointant à droite, de x0 à x1, centré verticalement.
        (x0..=x1).contains(&x) && (y - 0.5).abs() <= 0.34 * (x1 - x) / (x1 - x0)
    };
    let tri_left =
        |x0: f32, x1: f32| (x0..=x1).contains(&x) && (y - 0.5).abs() <= 0.34 * (x - x0) / (x1 - x0);
    let bar = |x0: f32, x1: f32| (x0..=x1).contains(&x) && (0.16..=0.84).contains(&y);
    match glyph {
        Glyph::Play => tri_right(0.28, 0.82),
        Glyph::Pause => bar(0.26, 0.42) || bar(0.58, 0.74),
        Glyph::Prev => bar(0.18, 0.28) || tri_left(0.30, 0.80),
        Glyph::Next => bar(0.72, 0.82) || tri_right(0.20, 0.70),
    }
}

/// Pixels BGRA (lissés par 4 × 4 échantillons).
fn pixels(glyph: Glyph) -> Vec<u8> {
    let mut out = vec![0u8; ICON_PX * ICON_PX * 4];
    for py in 0..ICON_PX {
        for px in 0..ICON_PX {
            let mut hits = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / ICON_PX as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / ICON_PX as f32;
                    hits += u32::from(inside(glyph, x, y));
                }
            }
            let alpha = (hits * 255 / 16) as u8;
            let i = (py * ICON_PX + px) * 4;
            // Prémultiplié : blanc × alpha.
            out[i..i + 4].copy_from_slice(&[alpha, alpha, alpha, alpha]);
        }
    }
    out
}

fn icon(glyph: Glyph) -> windows::core::Result<HICON> {
    let bits = pixels(glyph);
    let side = ICON_PX as i32;
    unsafe {
        let color = CreateBitmap(side, side, 1, 32, Some(bits.as_ptr().cast()));
        // Masque 1 bit, lignes alignées sur 16 bits : 4 octets par ligne de 20 px.
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
        // L'icône garde sa propre copie des bitmaps.
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_drawn() {
        for glyph in [Glyph::Prev, Glyph::Play, Glyph::Pause, Glyph::Next] {
            let lit = pixels(glyph).chunks(4).filter(|p| p[3] > 128).count();
            assert!(lit > 40, "dessin presque vide");
            assert!(lit < ICON_PX * ICON_PX / 2, "dessin trop plein");
        }
    }
}
