# ytmr

Client YouTube Music natif en Rust (Linux et Windows), avec une API locale pour le deck CYD (ESP32).

Projet en cours de développement : voir [`ytmr-plan.md`](ytmr-plan.md) pour le plan et [`CLAUDE.md`](CLAUDE.md) pour l'avancement.

## Prérequis

- Rust stable (1.85 minimum)
- Windows : Build Tools Visual Studio avec la charge de travail « Développement Desktop en C++ »
- `mpv` et `yt-dlp` dans le PATH, plus un moteur JavaScript pour yt-dlp (`deno` conseillé, `node` accepté)
  - Windows : `scoop bucket add extras && scoop install mpv yt-dlp deno`
  - CachyOS / Arch : `sudo pacman -S mpv yt-dlp deno`

## Utilisation (CLI)

```sh
ytmr-cli doctor                         # vérifie l'installation
ytmr-cli search daft punk
ytmr-cli play daft punk around the world
```

Pendant la lecture : `Entrée` ou `p` pause/reprise, `s 1:30` aller à, `+`/`-` volume, `v 50`, `i` état, `q` quitter.

## Licence

Au choix, [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE).

Usage personnel uniquement, dans le respect des conditions d'utilisation de YouTube.
