# ytmr — notes pour Claude Code

Client YouTube Music natif en Rust (CachyOS + Windows 11). Le plan de référence est `ytmr-plan.md` : l'architecture, les choix techniques et l'ordre des phases y sont fixés.

## Règles du projet

- **Une phase à la fois.** En fin de phase : résumé, commandes de validation, puis attendre l'accord de l'utilisateur.
- **Jamais d'extraction de flux YouTube** (signature, `n`, PO tokens). La lecture passe exclusivement par mpv (processus fils, JSON IPC) qui appelle yt-dlp.
- `ytmr-core` est la seule source de vérité. La GUI et l'API envoient des commandes et s'abonnent aux événements (`broadcast` pour les événements, `watch` pour l'état courant). Aucune logique métier dans la GUI ou l'API.
- Lecture derrière un trait async `Player`.
- **Pas de GUI iced** (abandonnée le 2026-10-29) : l'interface est une app web Svelte 5 + Vite (`web/`), embarquée dans le binaire, affichée dans une fenêtre native wry + tao qui charge l'UI servie par `ytmr-api`. L'UI ne parle qu'à l'API HTTP/WS, comme le deck CYD. Priorité : rapidité (jaquettes pré-redimensionnées et en cache côté Rust, bundle léger) et design soigné.
- N'inventer aucune API de crate : `cargo add`, puis vérifier les signatures dans `~/.cargo/registry/src` ou sur docs.rs (critique pour ytmapi-rs, souvlaki et iced).
- Pas de `unwrap()`/`expect()` hors tests (lint clippy du workspace). `thiserror` dans les libs, `anyhow` dans les binaires, logs avec `tracing`.
- `#[cfg(windows)]` / `#[cfg(unix)]` uniquement là où c'est inévitable (IPC mpv). Chemins via `directories` (`ytmr_core::AppPaths`).
- Chaque module non trivial commence par un commentaire `//!` qui explique son rôle en langage simple (« En clair : … »).
- API : `127.0.0.1:26540` par défaut ; token Bearer obligatoire hors localhost ; pas de CORS permissif ; bornes strictes (volume 0-100, seek, jaquette ≤ 320 px).

## Conventions

- Workspace Cargo, édition 2024, resolver 3. Les métadonnées et les lints sont hérités du workspace (`version.workspace = true`, `[lints] workspace = true`).
- Dépendances déclarées crate par crate (via `cargo add -p <crate>`).
- Code, commentaires et messages d'erreur en français ; identifiants en anglais.
- Commits : identité `noiseinsidemyhead1` / noreply (configurée dans `.git/config`), **pas de ligne Co-Authored-By**.

## Commandes

```sh
cargo build --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p ytmr-cli -- doctor                     # vérifie mpv, yt-dlp, moteur JS
cargo run -p ytmr-cli -- search daft punk           # recherche
cargo run -p ytmr-cli -- play daft punk around the world      # + radio (autoplay)
cargo run -p ytmr-cli -- play --all --no-autoplay daft punk     # tous les résultats en file
cargo run -p ytmr                                   # binaire GUI
RUST_LOG=ytmr_player::mpv::ipc=trace,ytmr_core=debug cargo run -p ytmr-cli -- play …   # IPC mpv + décisions du core
cargo test -p ytmr-catalog -- --ignored             # tests réseau
```

Sous Windows, compiler depuis PowerShell : dans Git Bash, `/usr/bin/link` masque le `link.exe` de MSVC.

**CI :** `.github/workflows/ci.yml` existe, mais GitHub Actions est bloqué sur le compte (`startup_failure` même sur un workflow vide, quota non épuisé). La validation de fin de phase se fait donc **en local sur Windows ET sur CachyOS** (`cargo build`, `clippy`, `test`).

## Pièges connus

- **Consentement RGPD** : depuis l'UE, `music.youtube.com` redirige vers `consent.youtube.com` (400, corps vide) et ytmapi-rs échoue avec « Unable to parse ytcfg ». `ytmr-catalog` envoie le cookie `SOCS=CAI` via un client reqwest passé à `YtMusicBuilder::new_with_client` (feature `reqwest` de ytmapi-rs).
- **Qualité contre vitesse (sans PO token, constaté le 2026-10-29)** : seuls les clients yt-dlp par défaut (TV/visionOS) donnent de l'audio seul (Opus 251, ~160 kb/s), mais l'URL n'est utilisable qu'à `available_at` (~5 s après la résolution ; avant, 403). Les clients `android`/`tv_simply`/`web_music` sont immédiats mais ne donnent que le format 18 (AAC ~96 kb/s). D'où `ytmr_player::resolve` : **préchargement** en `StartMode::Quality` (le délai est invisible), **démarrage à froid** en `StartMode::Fast` (`player.fast_start_client`, `android` par défaut). Tester un client : `yt-dlp -J -f bestaudio/best --extractor-args youtube:player_client=<c> <url>` et regarder `format_id` / `available_at`.
- **Premium** : avec `player.cookies_from_browser = "firefox"`, yt-dlp obtient le format 774 (Opus ~264 kb/s) ou 141 (AAC ~257 kb/s), sans délai `available_at`, mais ~7 s de résolution (requêtes authentifiées ; exporter les cookies dans un fichier ne fait rien gagner, mesuré). Le mode rapide est alors désactivé (les clients rapides n'acceptent pas les cookies). Chrome/Edge sous Windows : cookies chiffrés, illisibles par yt-dlp. Zen : `firefox:<dossier du profil>` n'a pas marché (base introuvable), à creuser si besoin.
- **Préchargement** : le core résout le suivant dès que le courant démarre, puis l'ajoute à la playlist de mpv (`loadfile … append`, `--prefetch-playlist`, `--gapless-audio`). mpv enchaîne seul et émet `start-file` avec le `playlist_entry_id` de l'entrée préparée. Mesuré : ~7 ms entre `end-file` et `playback-restart`. Un « suivant » vers une entrée préchargée = `playlist-next` (instantané). Si un préchargement en cours depuis > 5 s concerne le morceau demandé, on l'attend plutôt que de démarrer à froid.
- **User-Agent** : l'URL signée exige le User-Agent renvoyé par yt-dlp ; il est passé en option par fichier dans `loadfile` (`user-agent=%<octets>%<valeur>`, à cause des virgules).
- **Moteur JS** : yt-dlp n'active que deno par défaut ; si seul node est présent, ytmr ajoute `js-runtimes=node`.
- **stdin tokio** : la lecture de stdin bloque un thread ; la CLI termine avec `runtime.shutdown_background()` pour ne pas attendre un appui sur Entrée.
- **PATH** : après `scoop install`, les shells déjà ouverts ne voient pas mpv ; ouvrir un nouveau terminal.

## API (`ytmr-api`)

Base `http://127.0.0.1:26540/api/v1`. Lecture : `GET /state`, `POST /toggle|play|pause|next|prev`, `POST /seek {position_ms}`, `/volume {value}`, `/shuffle {on}`, `/repeat {mode: off|all|one}`, `/autoplay {on}`, `/like` (501 avant la phase 6). File : `GET /queue?offset&limit`, `POST /queue/play {tracks,start}`, `/queue/add {tracks}`, `/queue/next {tracks}`, `/queue/jump {id}`, `/queue/remove {id}`, `/queue/move {id,to}`, `/queue/clear`. Catalogue : `GET /search?q=`, `/album/{id}`, `/artist/{id}`. `POST /prefetch {video_id}` (résolution en avance au survol). Jaquettes : `GET /cover/{id}?size=&fmt=jpg|rgb565` (jpg ≤ 1200 px pour l'UI, rgb565 ≤ 320 px pour le CYD, `Cache-Control: immutable`), `GET /cover/{id}/accent`. `WS /ws` : `state`, `position` (1 Hz), `track_changed`, `queue_changed`, `error`.

Sécurité (`security.rs`) : `Host` limité aux noms de loopback en mode local (anti DNS rebinding), `Origin` étranger refusé (anti CSRF), token Bearer (ou `?token=`) dès que `bind` n'est pas loopback ou `require_token = true`. Les URL de jaquettes reçues des clients sont filtrées sur les domaines YouTube/Google (anti SSRF).

Mesuré (debug) : `/state` 342 o en < 1 ms ; jaquette 600 px 2,2 s la 1re fois (téléchargement + Lanczos), 1,7 ms depuis le cache.

## Emplacements à l'exécution

| | Linux | Windows |
|---|---|---|
| Config (`config.toml`) | `~/.config/ytmr/` | `%APPDATA%\ytmr\config\` |
| Cache | `~/.cache/ytmr/` | `%LOCALAPPDATA%\ytmr\cache\` |
| Logs (GUI) | `~/.local/share/ytmr/logs/` | `%APPDATA%\ytmr\data\logs\` |

## Avancement

| Phase | État |
|---|---|
| 0 — Socle (workspace, lints, CI, tracing, config) | validée sous Windows ; à vérifier sous CachyOS |
| 1 — Lecture headless (catalogue, mpv IPC, CLI) | validée sous Windows ; à vérifier sous CachyOS |
| 2 — Core et file d'attente (préchargement, radio) | validée sous Windows ; à vérifier sous CachyOS |
| 3 — API et jaquettes | validée sous Windows (curl, WebSocket, RGB565 décodée) |
| 4 — Interface web (Svelte 5 + Vite, fenêtre wry/tao) | à faire |
| 5 — Firmware CYD | à faire |
| 6 — Authentification et bibliothèque | à faire |
| 7 — Intégration OS | à faire |
| 8 — Packaging | à faire |
