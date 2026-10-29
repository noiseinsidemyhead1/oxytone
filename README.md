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
ytmr-cli play daft punk around the world        # puis radio automatique
ytmr-cli play --all --no-autoplay daft punk       # tous les résultats en file
```

Pendant la lecture : `Entrée`/`p` pause, `n` suivant, `b` précédent, `s 1:30` aller à, `+`/`-` ou `v 50` volume, `z` shuffle, `r` repeat, `a` autoplay, `l` file, `i` état, `q` quitter.

Le morceau suivant est préparé pendant la lecture du courant : l'enchaînement est sans blanc (~7 ms mesurés) et en qualité haute (Opus). Seul un démarrage à froid (premier morceau, saut vers un morceau pas encore préparé) passe par un flux plus rapide à obtenir mais de qualité moindre ; réglable avec `player.fast_start_client` dans `config.toml` (vide = toujours la qualité, quelques secondes d'attente en plus).

## Licence

Au choix, [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE).

Usage personnel uniquement, dans le respect des conditions d'utilisation de YouTube.
