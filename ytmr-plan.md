# ytmr — client YouTube Music natif en Rust

Nom de travail : `ytmr` (à changer librement).

## 1. Est-ce pertinent ?

Oui, si les objectifs sont les suivants :

- une appli native légère (cible < 80 Mo de RAM au repos, contre plusieurs centaines pour Pear/Electron) ;
- une API sur mesure pour le deck CYD, sans contrainte héritée de Pear ;
- la suppression du relais Python pour les jaquettes : le serveur Rust les sert directement, redimensionnées et au format natif de l'écran ;
- un projet Rust complet à mettre en avant (async, GUI, réseau, intégration OS).

Non, si le seul but est d'écouter de la musique : Pear fonctionne déjà.

Règle d'or : **on ne touche pas à l'extraction des flux**. Signature, paramètre `n` et PO tokens sont délégués à mpv + yt-dlp. Il suffira de lancer `yt-dlp -U` de temps en temps.

> En clair : YouTube change régulièrement la serrure qui protège ses fichiers audio. yt-dlp a une équipe qui refait la clé à chaque fois. On utilise leur clé au lieu de forger la nôtre.

## 2. Architecture

```
ytmr/                      (workspace Cargo)
├── crates/
│   ├── ytmr-core/        état du lecteur, file d'attente, bus d'événements
│   ├── ytmr-catalog/     wrapper autour de ytmapi-rs (recherche, albums, playlists, radio)
│   ├── ytmr-player/      trait Player + implémentation mpv (JSON IPC)
│   ├── ytmr-api/         serveur HTTP + WebSocket (axum) pour le CYD et autres clients
│   ├── ytmr-media/       jaquettes : cache disque, resize, conversion RGB565
│   ├── ytmr-os/          MPRIS (Linux) / SMTC (Windows) via souvlaki
│   └── ytmr-gui/         interface native iced (rendu logiciel tiny-skia) + assemblage de l'appli
└── apps/
    ├── ytmr/             binaire GUI (assemble tout)
    └── ytmr-cli/         binaire headless (debug, usage sans GUI)
```

### Choix techniques

| Besoin | Choix | Pourquoi |
|---|---|---|
| Catalogue | `ytmapi-rs` | API interne YouTube Music en pur Rust, async tokio, auth cookie ou OAuth |
| Lecture | `mpv` en processus fils piloté via JSON IPC | mpv appelle yt-dlp tout seul, gère le buffering, le gapless et les formats ; aucun linkage C pénible sous Windows ; un crash de mpv ne tue pas l'appli |
| Abstraction lecture | trait `Player` | permet de passer à `libmpv2` (intégré) ou `rodio` plus tard sans toucher au reste |
| Async | `tokio` | standard, requis par ytmapi-rs et axum |
| API | `axum` (HTTP + WebSocket) | léger, idiomatique |
| GUI | `iced` 0.14, rendu logiciel `tiny-skia` | pur Rust, architecture Elm ; la webview (WebView2) a été essayée puis écartée le 2026-10-29 : ~133 Mo pour une page vide. Le rendu GPU d'iced coûte 90 à 190 Mo de pilote graphique ; tiny-skia ~21 Mo pour 0,1 % de CPU |
| Intégration OS | `souvlaki` | une seule API pour MPRIS (Linux) et SMTC (Windows) : touches média, overlay Windows |
| Images | `image` + cache disque | resize et encodage des jaquettes |
| Config / chemins | `serde` + `toml` + `directories` | `~/.config/ytmr` sous Linux, `%APPDATA%\ytmr` sous Windows |
| Logs | `tracing` + `tracing-subscriber` | logs structurés, fichier rotatif |

> En clair : mpv est un lecteur vidéo/audio en ligne de commande très solide. On le lance en arrière-plan et on lui parle par un petit canal (un socket sous Linux, un named pipe sous Windows), comme une télécommande.

### Flux de données

```
GUI (iced) ──────────commandes──────────▶ core ◀──commandes── API (axum) ◀── CYD / téléphone
                            │
              événements (tokio::broadcast)
                            │
        ┌──────────┬────────┴─────┬───────────┐
        ▼          ▼              ▼           ▼
     player     catalog         media        os
     (mpv)    (ytmapi-rs)     (jaquettes)  (souvlaki)
```

Le `core` est la seule source de vérité. La GUI (en direct, dans le même processus) et l'API (pour le CYD) ne sont que deux télécommandes.

## 3. API pour le CYD (nouvelle, sur mesure)

Écoute sur `127.0.0.1:26540` par défaut. L'écoute LAN est optionnelle (`bind = "0.0.0.0"`) et impose alors un token.

| Méthode | Route | Rôle |
|---|---|---|
| GET | `/api/v1/state` | état compact : titre, artiste, album, durée, position, lecture, volume, shuffle, repeat, liked, `cover_id` |
| POST | `/api/v1/toggle` · `/play` · `/pause` · `/next` · `/prev` | contrôles |
| POST | `/api/v1/seek` `{ "position_ms": 42000 }` | seek |
| POST | `/api/v1/volume` `{ "value": 0-100 }` | volume |
| POST | `/api/v1/like` | like / unlike |
| GET | `/api/v1/queue` | file d'attente (paginée) |
| GET | `/api/v1/cover/{cover_id}?size=120&fmt=rgb565\|jpg` | jaquette redimensionnée ; `rgb565` = octets bruts little-endian, directement blittables par LVGL |
| WS | `/api/v1/ws` | push : `track_changed`, `state_changed`, `position` (1 Hz) |

Contraintes ESP32 : JSON < 1 Ko pour `/state`, pas de champs imbriqués inutiles, noms de champs courts mais lisibles, HTTP simple (pas de HTTPS requis en local).

Auth : en-tête `Authorization: Bearer <token>`, généré au premier lancement et stocké dans la config. Il est ignoré si `bind = 127.0.0.1` et `require_token = false`.

> En clair : au lieu que l'ESP32 télécharge une image JPEG et galère à la décoder, le PC lui envoie l'image déjà « mâchée » : pixels prêts à afficher, à la bonne taille. Le relais Python devient inutile.

## 4. Phases

Chaque phase se termine par un critère de validation concret. On ne passe pas à la suivante avant.

**Phase 0 — Socle**
Workspace, `rustfmt`, `clippy -D warnings`, CI GitHub Actions (Linux + Windows), `tracing`, chargement de la config.
Validation : `cargo build` et `cargo clippy` passent sur les deux OS en CI.

**Phase 1 — Lecture headless**
`ytmr-catalog` (recherche non authentifiée) et `ytmr-player` (mpv IPC). CLI : `ytmr-cli play "daft punk around the world"`.
Validation : le son sort sous CachyOS et Windows, pause/reprise/seek fonctionnent depuis la CLI.

**Phase 2 — Core et file d'attente**
Machine à états, file d'attente, shuffle, repeat, autoplay (radio via la watch playlist), bus d'événements, préchargement du morceau suivant.
Validation : tests unitaires sur la file d'attente ; enchaînement automatique de 3 morceaux sans trou notable.

**Phase 3 — API et jaquettes**
`ytmr-api` + `ytmr-media` (cache, resize, RGB565).
Validation : `curl` sur toutes les routes, client WS de test (`websocat`), une jaquette RGB565 décodée correctement par un petit script de vérification.

**Phase 4 — GUI native iced** *(ex-phase 6, avancée à la demande de l'utilisateur le 2026-10-29)*
Vues : accueil « en cours » (grande jaquette, teinte tirée de la pochette, à suivre), recherche (morceaux, albums, artistes), album, artiste, file d'attente, barre de lecture. Jaquettes décodées hors du fil d'affichage, préparation du morceau survolé ou de la page ouverte.
Validation : navigation fluide, jaquettes instantanées depuis le cache, mesures RAM/CPU notées dans le README.

**Phase 5 — Firmware CYD**
Adapter le firmware à la nouvelle API (WS pour l'état, HTTP pour les commandes et la jaquette RGB565) et supprimer le relais Python.
Validation : le deck affiche titre, progression et jaquette, et contrôle la lecture.

**Phase 6 — Authentification et bibliothèque**
Cookie navigateur (`cookie.txt`), playlists, likes, historique, bibliothèque.
Validation : les playlists perso s'affichent et se lisent.

**Phase 7 — Intégration OS**
souvlaki (MPRIS / SMTC), touches média, notification de changement de piste, icône de tray (optionnelle).
Validation : `playerctl` fonctionne sous Linux, l'overlay média Windows affiche le morceau.

**Phase 8 — Packaging**
PKGBUILD (AUR), archive Windows (binaire + `mpv.exe` + `yt-dlp.exe`, ou détection dans le PATH), README d'installation.

## 5. Objectifs de performance

| Mesure | Cible |
|---|---|
| RAM au repos (GUI ouverte) | < 80 Mo (mpv compris : < 120 Mo) |
| Démarrage à froid jusqu'à l'UI | < 400 ms |
| Recherche (hors réseau) | rendu < 16 ms après réception |
| Latence API locale `/state` | < 5 ms |

Le temps avant que le son démarre dépend surtout du réseau et de yt-dlp (~0,5 à 2 s). Le préchargement du morceau suivant masque ce délai entre deux pistes.

## 6. Risques

- **Changements côté YouTube.** Pour la lecture, il suffit de mettre à jour yt-dlp. Pour le catalogue, il faut mettre à jour `ytmapi-rs` ; isoler ce crate derrière `ytmr-catalog` limite l'impact.
- **CGU YouTube.** Usage perso uniquement, pas de redistribution de contenu.
- **Exposition réseau de l'API.** Écoute en localhost par défaut, token obligatoire en LAN, pas de CORS ouvert.

## 7. Prompt Claude Code

À coller dans Claude Code depuis un dossier vide.

```text
Tu vas développer "ytmr", un client YouTube Music natif en Rust, multiplateforme (Linux CachyOS et Windows 11). Le plan complet est dans ytmr-plan.md à la racine : lis-le entièrement avant toute chose et respecte l'architecture, les choix techniques et l'ordre des phases.

Règles de travail :
1. Commence par créer CLAUDE.md : résumé du projet, conventions, commandes utiles (build, test, clippy, lancement), état d'avancement par phase. Mets-le à jour à la fin de chaque phase.
2. Travaille UNE phase à la fois. À la fin de chaque phase, arrête-toi, résume ce qui a été fait, donne-moi les commandes exactes pour valider le critère de la phase, et attends mon accord avant de continuer.
3. N'invente aucune API de crate. Ajoute les dépendances avec `cargo add` (dernières versions stables), puis vérifie les signatures réelles via docs.rs ou `cargo doc --open` / le code source dans ~/.cargo/registry avant de les utiliser. C'est critique pour ytmapi-rs, souvlaki et iced, dont les API évoluent vite.
4. Ne réimplémente JAMAIS l'extraction des flux YouTube (signature, paramètre n, PO tokens). La lecture passe exclusivement par mpv lancé en processus fils, piloté par JSON IPC (socket Unix sous Linux, named pipe \\.\pipe\ytmr-mpv sous Windows), mpv utilisant yt-dlp. Vérifie au démarrage que mpv et yt-dlp sont présents et donne un message d'erreur clair sinon.
5. La lecture est derrière un trait `Player` (async) pour pouvoir changer d'implémentation plus tard.
6. Le crate core est la seule source de vérité. La GUI et l'API envoient des commandes et s'abonnent aux événements (tokio::sync::broadcast + watch pour l'état courant). Aucune logique métier dans la GUI ou l'API.
7. Qualité : `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` et `cargo test --workspace` doivent passer à chaque fin de phase. Pas de `unwrap()` hors tests ; erreurs avec `thiserror` dans les libs et `anyhow` dans les binaires. Logs avec `tracing`.
8. Code multiplateforme : utilise `directories` pour les chemins et `#[cfg(windows)]` / `#[cfg(unix)]` uniquement là où c'est inévitable (IPC mpv). Ajoute dès la phase 0 une CI GitHub Actions qui build et teste sur ubuntu-latest et windows-latest.
9. Sécurité de l'API : écoute sur 127.0.0.1:26540 par défaut ; si bind != localhost, token Bearer obligatoire (généré aléatoirement au premier lancement, stocké dans la config) ; pas de CORS permissif ; validation stricte des entrées (bornes du volume, du seek, de la taille des jaquettes : max 320).
10. Jaquettes pour l'ESP32 : la route /api/v1/cover/{id}?size=N&fmt=rgb565 renvoie des pixels RGB565 bruts little-endian (N*N*2 octets), avec Content-Type application/octet-stream et en-têtes X-Width / X-Height. Mets en cache sur disque par (id, taille, format).
11. Mesure les performances à la phase 6 (RAM au repos, temps de démarrage) et note les résultats dans le README.
12. Pour chaque module non trivial, ajoute en tête un court commentaire expliquant son rôle en langage simple, en plus de la doc technique.

Commence par la phase 0.
```

### Notes pour la phase 4 (firmware CYD)

Pistes à donner à Claude Code au moment voulu :

- WebSocket côté ESP32 avec la bibliothèque `arduinoWebSockets` (Links2004) ;
- jaquette RGB565 de 120x120 = 28,8 Ko, à lire dans un buffer PSRAM ou statique puis à passer à un `lv_img_dsc_t` (`LV_IMG_CF_TRUE_COLOR`, `LV_COLOR_DEPTH 16`) ;
- vérifier `LV_COLOR_16_SWAP` dans `lv_conf.h` et l'ordre d'octets choisi côté serveur ; prévoir un paramètre `&swap=1` sur la route si nécessaire.
