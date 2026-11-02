# oxytone

Client YouTube Music natif en Rust (Linux et Windows), avec une API locale pour le deck CYD (ESP32).

Projet en cours de développement : voir [`oxytone-plan.md`](oxytone-plan.md) pour le plan et [`CLAUDE.md`](CLAUDE.md) pour l'avancement.

## Installation

oxytone se sert de trois outils installés à part : **mpv** (lecture), **yt-dlp**
(adresses des flux) et **node** (défis JavaScript de YouTube pour yt-dlp).
**Python** est conseillé : yt-dlp reste alors en mémoire et un morceau démarre
plus vite. Au premier lancement, oxytone dit ce qui manque et comment l'installer.

### Windows

1. Les outils (une fois) : `scoop bucket add extras && scoop install mpv yt-dlp nodejs python`
2. L'installateur `oxytone-setup-<version>.exe` : installation pour toi seul (sans
   droits administrateur) ou pour tous, raccourci du menu Démarrer, raccourci
   sur le Bureau et lancement au démarrage de Windows au choix. Désinstallation
   depuis « Applications installées » (réglages et téléchargements gardés, ou
   tout effacé si tu le demandes).

**Depuis ytmr** (l'ancien nom) : l'installateur ferme ytmr (il demande de le
quitter par son icône près de l'horloge, pour garder la file), le désinstalle
sans toucher à ses données, puis oxytone les reprend à son premier lancement
(réglages, session, téléchargements, statistiques, file), sans rien
re-télécharger.

Fabriquer l'installateur (Inno Setup requis : `scoop install inno-setup`) :

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1
# oxytone ouvert ? target\release est verrouillé :
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 -TargetDir target\alt
```

Résultat : `dist\oxytone-setup-<version>.exe`.

### CachyOS / Arch

```sh
cd packaging/linux && makepkg -si
```

Le paquet `oxytone-git` installe mpv, yt-dlp et node en dépendances, puis oxytone,
son icône et son entrée dans le menu des applications.

### Depuis les sources

- Rust stable (1.88 minimum) ; sous Windows, les Build Tools Visual Studio
  (« Développement Desktop en C++ »).
- `cargo run --release -p oxytone`

## Utilisation

Raccourcis (aide complète avec `?`) : `Espace` lecture/pause, `←`/`→` ±5 s,
`Ctrl+←`/`Ctrl+→` précédent/suivant, `↑`/`↓` volume, `M` muet, `L` like,
`Ctrl+F` rechercher, `Ctrl+Z` remettre un morceau retiré de la file, `F11`
plein écran, `Échap` fermer / remonter en haut de la page.

Compte YouTube (playlists, likes) et qualité Premium (Opus ~256 kb/s) :
bouton « Se connecter à YouTube » sous Windows (fenêtre de connexion Google,
session chiffrée par Windows) ; sous Linux, dans `config.toml`, section
`[player]` : `cookies_from_browser = "firefox"` (YouTube connecté dans Firefox).

## Performances mesurées

Windows 11, build release, 2026-10-29 :

| Mesure | Résultat |
|---|---|
| Mémoire privée de l'appli (rendu GPU, carte intégrée) | ~110 Mo (+ mpv ~38 Mo) |
| Même chose en rendu logiciel (`ICED_BACKEND=tiny-skia`) | ~21 Mo, mais moins fluide |
| Clic « Lecture » sur un album ouvert → son | 0,4 s (morceau préparé à l'ouverture) |
| Enchaînement entre deux morceaux | ~7 ms de silence |
| Skip vers l'un des 10 morceaux suivants (déjà préparés) | ~0,06 s |
| Premier morceau sans préparation, jusqu'au son (2026-11-02) | ~1,5 s (Premium, Opus 774 ; sans compte, AAC 18) |
| `/api/v1/state` | 342 octets, < 1 ms |

Le rendu passe par la carte graphique intégrée (DirectX 12 sous Windows), synchronisé sur la fréquence de l'écran. Le rendu logiciel économise ~90 Mo (le pilote graphique) mais saccade au défilement : `ICED_BACKEND=tiny-skia` pour l'essayer.

## Utilisation (CLI)

```sh
oxytone-cli doctor                      # vérifie l'installation
oxytone-cli search daft punk
oxytone-cli play daft punk around the world     # puis radio automatique
oxytone-cli play --all --no-autoplay daft punk    # tous les résultats en file
oxytone-cli bench-cold                  # mesure du démarrage à froid
```

Pendant la lecture : `Entrée`/`p` pause, `n` suivant, `b` précédent, `s 1:30` aller à, `+`/`-` ou `v 50` volume, `z` shuffle, `r` repeat, `a` autoplay, `l` file, `i` état, `q` quitter.

Le morceau suivant est préparé pendant la lecture du courant : l'enchaînement est sans blanc (~7 ms mesurés) et en qualité haute (Opus). Seul un démarrage à froid (premier morceau, saut vers un morceau pas encore préparé) passe par un flux plus rapide à obtenir mais de qualité moindre ; réglable avec `player.fast_start_client` dans `config.toml` (vide = toujours la qualité, quelques secondes d'attente en plus).

## Licence

Au choix, [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE).

Usage personnel uniquement, dans le respect des conditions d'utilisation de YouTube.
