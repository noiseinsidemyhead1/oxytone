# ytmr — notes pour Claude Code

Client YouTube Music natif en Rust (CachyOS + Windows 11). Le plan de référence est `ytmr-plan.md` : l'architecture, les choix techniques et l'ordre des phases y sont fixés.

## Règles du projet

- **Une phase à la fois.** En fin de phase : résumé, commandes de validation, puis attendre l'accord de l'utilisateur.
- **Jamais d'extraction de flux YouTube** (signature, `n`, PO tokens). La lecture passe exclusivement par mpv (processus fils, JSON IPC) qui appelle yt-dlp.
- `ytmr-core` est la seule source de vérité. La GUI et l'API envoient des commandes et s'abonnent aux événements (`broadcast` pour les événements, `watch` pour l'état courant). Aucune logique métier dans la GUI ou l'API.
- Lecture derrière un trait async `Player`.
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
cargo run -p ytmr-cli            # binaire headless
cargo run -p ytmr                # binaire GUI
RUST_LOG=debug cargo run -p ytmr-cli
```

Sous Windows, compiler depuis PowerShell : dans Git Bash, `/usr/bin/link` masque le `link.exe` de MSVC.

## Emplacements à l'exécution

| | Linux | Windows |
|---|---|---|
| Config (`config.toml`) | `~/.config/ytmr/` | `%APPDATA%\ytmr\config\` |
| Cache | `~/.cache/ytmr/` | `%LOCALAPPDATA%\ytmr\cache\` |
| Logs (GUI) | `~/.local/share/ytmr/logs/` | `%APPDATA%\ytmr\data\logs\` |

## Avancement

| Phase | État |
|---|---|
| 0 — Socle (workspace, lints, CI, tracing, config) | en validation |
| 1 — Lecture headless | à faire |
| 2 — Core et file d'attente | à faire |
| 3 — API et jaquettes | à faire |
| 4 — Firmware CYD | à faire |
| 5 — Authentification et bibliothèque | à faire |
| 6 — GUI iced | à faire |
| 7 — Intégration OS | à faire |
| 8 — Packaging | à faire |
