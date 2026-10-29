# ytmr — notes pour Claude Code

Client YouTube Music natif en Rust (CachyOS + Windows 11). Le plan de référence est `ytmr-plan.md` : l'architecture, les choix techniques et l'ordre des phases y sont fixés.

## Règles du projet

- **Une phase à la fois.** En fin de phase : résumé, commandes de validation, puis attendre l'accord de l'utilisateur.
- **Jamais d'extraction de flux YouTube** (signature, `n`, PO tokens). La lecture passe exclusivement par mpv (processus fils, JSON IPC) ; les adresses viennent de yt-dlp. Seule exception assumée : le cache audio télécharge l'adresse que yt-dlp a donnée (rien n'est déchiffré par ytmr).
- `ytmr-core` est la seule source de vérité. La GUI et l'API envoient des commandes et s'abonnent aux événements (`broadcast` pour les événements, `watch` pour l'état courant). Aucune logique métier dans la GUI ou l'API.
- Lecture derrière un trait async `Player`.
- **GUI native iced 0.14** (`ytmr-gui`), rendu logiciel tiny-skia par défaut. Une interface web (webview) a été essayée et **refusée par l'utilisateur** (« une autre app pas performante ») : ne pas y revenir. La GUI parle directement au `CoreHandle` (même processus) ; l'API ne sert qu'au CYD et aux télécommandes. Priorités de l'utilisateur : rapidité, légèreté, design soigné avec jaquettes.
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
cargo run --release -p ytmr                         # l'appli (le debug est lent pour le décodage d'images)
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
- **Anticipation** : le core résout en file de fond (`Priority::Background`, 2 yt-dlp) les 10 morceaux après le suivant ; le suivant et les clics passent en `Priority::Urgent` (2 autres yt-dlp). Une vidéo déjà en cours de résolution est partagée (`inflight` dans `YtDlpResolver`), jamais relancée. Cache d'adresses plafonné à 300 entrées + expiration ; jaquettes sur disque purgées au démarrage au-delà de 250 Mo. Mesuré : skip vers un morceau anticipé ≈ 0,06 s ; 1er morceau à froid ≈ 8 s (Premium). Grouper plusieurs URL dans un seul yt-dlp ne fait gagner que le démarrage (~1,2 s, exe PyInstaller) : ~5 s par morceau restent côté YouTube.
- **Worker yt-dlp** (`ytmr_player::worker`) : processus Python permanent qui importe la bibliothèque yt-dlp (environnement privé `cache_dir/ytdlp-env`, créé et mis à jour une fois par jour par `ensure_env`, ~32 s la toute première fois). Le résolveur bascule dessus dès qu'il est prêt, sinon (ou en cas d'échec) il utilise l'exécutable. Avec cookies, client `web_music` + `player_skip=webpage,configs` (même format 774). Mesuré : ~3 s par morceau (worker) / ~5,5 s (exécutable) / ~7,2 s avant ces optimisations. Tout yt-dlp de fond tourne en priorité basse (`process::below_normal_priority`). Adresses résolues gardées dans `cache_dir/streams.json` (valables plusieurs heures) : au relancement, la session reprise et les 10 suivants sont prêts.
- **Cache audio** (`ytmr_player::audio_cache`) : chaque morceau résolu en qualité haute est téléchargé en fond (un à la fois, morceaux de 1 Mo avec `Range` et le User-Agent de yt-dlp, taille attendue lue dans `clen=`) vers `cache_dir/audio/<id>.audio` ; `Resolver::cached` le rend en premier → mpv lit le fichier, ni yt-dlp ni réseau. Limite `player.audio_cache_mb` (2 048 par défaut, 0 = désactivé), les moins récemment écoutés (date de modification, touchée à chaque lecture) partent en premier. Mesuré : 8,8 Mo en 1,6 s.
- **Effets audio** (`ytmr_player::effects`) : égaliseur 10 bandes ±12 dB + `dynaudnorm`, traduits en propriété `af` de mpv (`lavfi=[volume=-XdB,equalizer=…,dynaudnorm=…]`, baisse préalable du gain maximal pour ne pas saturer). `Command::SetEffects`, gardés dans la session.
- **Session** : `data_dir/session.json` (file ≤ 500 titres, morceau courant, position, volume, repeat, radio, effets), écrite à l'arrêt du moteur, reprise au lancement (en pause, même position ; « lecture » relance et rejoint la position). Le courant d'une session reprise est résolu d'avance.
- **User-Agent** : l'URL signée exige le User-Agent renvoyé par yt-dlp ; il est passé en option par fichier dans `loadfile` (`user-agent=%<octets>%<valeur>`, à cause des virgules).
- **Fenêtres console** : un programme console (yt-dlp, raccourcis scoop) lancé depuis l'appli fenêtrée ouvre un terminal noir. Toujours passer par `ytmr_player::process::command` (`CREATE_NO_WINDOW`).
- **Moteur JS** : yt-dlp n'active que deno par défaut ; si seul node est présent, ytmr ajoute `js-runtimes=node`.
- **stdin tokio** : la lecture de stdin bloque un thread ; la CLI termine avec `runtime.shutdown_background()` pour ne pas attendre un appui sur Entrée.
- **PATH** : après `scoop install`, les shells déjà ouverts ne voient pas mpv ; ouvrir un nouveau terminal.

## API (`ytmr-api`)

Base `http://127.0.0.1:26540/api/v1`. Lecture : `GET /state`, `POST /toggle|play|pause|next|prev`, `POST /seek {position_ms}`, `/volume {value}`, `/shuffle {on}`, `/repeat {mode: off|all|one}`, `/autoplay {on}`, `/like` (501 avant la phase 6). File : `GET /queue?offset&limit`, `POST /queue/play {tracks,start}`, `/queue/add {tracks}`, `/queue/next {tracks}`, `/queue/jump {id}`, `/queue/remove {id}`, `/queue/move {id,to}`, `/queue/clear`. Catalogue : `GET /search?q=`, `/album/{id}`, `/artist/{id}`. `POST /prefetch {video_id}` (résolution en avance au survol). Jaquettes : `GET /cover/{id}?size=&fmt=jpg|rgb565` (jpg ≤ 1200 px pour l'UI, rgb565 ≤ 320 px pour le CYD, `Cache-Control: immutable`), `GET /cover/{id}/accent`. `WS /ws` : `state`, `position` (1 Hz), `track_changed`, `queue_changed`, `error`.

Sécurité (`security.rs`) : `Host` limité aux noms de loopback en mode local (anti DNS rebinding), `Origin` étranger refusé (anti CSRF), token Bearer (ou `?token=`) dès que `bind` n'est pas loopback ou `require_token = true`. Les URL de jaquettes reçues des clients sont filtrées sur les domaines YouTube/Google (anti SSRF).

Mesuré (debug) : `/state` 342 o en < 1 ms ; jaquette 600 px 2,2 s la 1re fois (téléchargement + Lanczos), 1,7 ms depuis le cache.

## GUI (`ytmr-gui`)

- `app.rs` : état + `update` (architecture Elm) ; `views.rs` : widgets ; `style.rs` : palette et styles ; `icons.rs` : icônes SVG en ligne ; `backend.rs` : démarrage/arrêt de mpv, catalogue, core, API.
- La fenêtre s'ouvre tout de suite ; le backend démarre dans un `Task` (`Message::BackendReady`). Fermer la fenêtre (`exit_on_close_request: false`) arrête d'abord mpv proprement.
- Les changements du core arrivent par `Task::stream(core_updates)` (watch + broadcast) : `Subscription::run_with` exige des données `Hash`.
- Jaquettes : `Covers::get_rgba` décode en tâche de fond, `image::Handle::from_rgba` ; cache `(cover_id, taille)` dans `App`. Teinte de page : `Covers::accent` puis `style::accent_from_rgb`.
- Préchargement : survol d'un morceau > 450 ms, premier résultat de recherche et premier morceau d'un album ouvert → `Command::Prefetch`. Mesuré : clic « Lecture » → son en 0,4 s.
- **Rendu** (`apps/ytmr/src/main.rs::configure_renderer`) : **GPU (wgpu) par défaut**, DX12 + carte basse consommation, vsync (suit la fréquence de l'écran). L'utilisateur veut ≥ 60 im/s et aucune saccade : tiny-skia (essayé en premier) saccadait au survol et au défilement et dessinait mal les ombres floues (halo carré autour du bouton lecture). Coût : ~110 Mo au lieu de ~21 Mo. Historique des mesures : Mesuré (release, 2026-10-29) : ~21 Mo privés au repos, ~37 Mo après navigation, 0,1 % CPU en lecture ; avec le GPU : 109 Mo (iGPU DX12) à 207 Mo (RTX 5060). Si GPU : `WGPU_BACKEND=dx12` sous Windows (OpenGL fait planter le démarrage : 0xC0000005) et `WGPU_POWER_PREF=low`. `set_var` est `unsafe` en Rust 2024 : lint `unsafe_code = "deny"` au workspace, autorisé seulement là.
- iced 0.14 exige Rust 1.88 (`rust-version` du workspace) ; `cargo add` choisit sinon la 0.13.
- Ombres floues + rendu logiciel = petits points clairs aux coins : pas d'ombre floue sur les grandes pochettes.
- **Défilement** : iced fait sauter les listes de 60 px par cran de molette (pas d'animation) ; `smooth.rs` intercepte les crans (`WheelCatcher`) et anime la position via `window::frames()` (abonnement actif seulement pendant l'animation). Le pavé tactile (`ScrollDelta::Pixels`) n'est pas intercepté.
- Tests visuels sous Windows : scripts PowerShell (capture via `DwmGetWindowAttribute` + `CopyFromScreen`, clics via `SetCursorPos`/`mouse_event`) **avec `SetProcessDPIAware()`**, sinon les coordonnées sont décalées de 125 %. Ne jamais simuler clavier/souris quand l'utilisateur se sert de l'appli.

## Compte YouTube (`ytmr-catalog::account`, `ytmr-gui::library`)

- Cookies de la session Firefox lus **par le worker** (`Worker::browser_cookie`, `yt_dlp.cookies.extract_cookies_from_browser`), gardés en mémoire seulement, passés à ytmapi-rs (`with_browser_token_cookie`). `Account` a un `Debug` manuel pour ne jamais logger le jeton. Sans worker (pas de Python, options yt-dlp personnalisées), pas de compte.
- Identifiants de playlist : lecture = `VL<id>` (navigation), modifications = id nu. ytmr manipule l'id nu (`bare_id`, `browse_id`, `edit_id`). Titres likés : `LM`. Listes de podcasts `RDPN…` écartées.
- ytmapi-rs 0.3.3 contourné en lisant le **JSON brut** : (1) pas de `setVideoId` dans ses morceaux (exigé pour retirer) → `playlistItemData.playlistSetVideoId` ; (2) son décodeur rejette une page entière si un morceau n'a pas de page artiste → décodeur tolérant `collect_tracks` ; (3) sa pagination ré-encode le jeton de continuation (`%3D` → `%253D`), donc s'arrête à la 1re page → requête maison `NextPage` (jeton décodé, `type=next`, une relance en cas d'échec). Mesuré : 1 188/1 188 titres et 908 likes (yt-dlp en voit 943, avec les indisponibles).
- GUI : modifications optimistes (affichage immédiat, correction + toast si YouTube refuse). Boutons des lignes visibles au survol.
- **Affichage des playlists** : les pages YouTube (100 titres) ne peuvent venir que l'une après l'autre ; `Account::playlist_progressive` envoie chaque page dès réception, la GUI l'affiche au fil de l'eau. Cache disque `cache_dir/library/` (`playlists.json`, `playlist-<id>.json`) : version en cache affichée tout de suite, version fraîche construite à côté puis substituée. Rendu limité à `ROWS_STEP` (120) lignes, +120 en approchant du bas (pochettes demandées seulement pour les lignes affichées).
- **Virtualisation** : lignes de hauteur fixe (`ROW_H` 58, `QROW_H` 50, texte sur une ligne via `Wrapping::None` + `clip`), en-tête de playlist de hauteur fixe (`PLAYLIST_HEADER_H`), `visible_range` + `virtual_list` (espaces vides au-dessus/en dessous). Position suivie par `Message::Scrolled` (`page_view`, `queue_view`). Pochettes demandées seulement pour les lignes visibles, 120 ms après l'arrêt du défilement (`CoversSettle`). **Ne jamais cloner une liste dans le message d'une ligne** (`Message::Play(tracks.clone(), i)` dans 1 189 lignes = 1,4 M de copies par image : c'était la cause des 0 im/s) → `Lib::PlayAt(i)`.
- **Vignettes** : source téléchargée en 240 px pour ≤ 200 px demandés (1 200 px sinon), RGBA produit directement, filtre CatmullRom, 2 décodages à la fois max.
- **Rechargements** : playlist et liste des playlists 30 min, likes 1 h, historique 10 min (3 min après une écoute) — âge = date du fichier de cache ; bouton ↻ pour forcer. Les modifications locales réécrivent le cache.
- **Mise en page** : barres latérales redimensionnables (`DragStart`/`DragMove`/`DragEnd`, `event::listen_with` pendant le glissement), largeurs et file visible gardées dans `cache_dir/ui.json`. Accueil = salutation + sections du compte ; la grande vue « En cours » (`Route::NowPlaying`) s'ouvre depuis la pochette/titre de la barre du bas. Shift + molette n'est pas intercepté par `WheelCatcher` (défilement horizontal des rangées).
- **Décodeurs de ytmapi-rs trop stricts** : la recherche de titres (« Expected Song result ») et l'en-tête des playlists des autres (« straplineTextOne / facepile ») sont lus en JSON brut (`ytmr_catalog::raw`, requête `RawPost`). L'en-tête d'une playlist vient de sa 1re page (une requête de moins) et dit si elle appartient au compte (`musicEditablePlaylistDetailHeaderRenderer` → `Playlist::owned`) : sinon ni renommage, ni suppression, ni retrait. Sous une playlist courte ou vide, YouTube ajoute des **suggestions** (`musicShelfRenderer` / `musicCarouselShelfRenderer`, avec leur propre continuation) : elles sont ignorées (une playlist neuve « contenait » 7 titres).
- **Démarrage sans réseau** : `Catalog::new()` ne se connecte qu'à la première utilisation (`OnceCell`, 3 essais) ; avant, une requête ratée vers music.youtube.com au lancement empêchait l'appli de s'ouvrir.
- **Profil** : pas d'API dans ytmapi-rs → requête maison `AccountMenu` (`account/account_menu`), `activeAccountHeaderRenderer` (nom, @pseudo, photo). **Historique** : `GetHistoryQuery` en JSON brut + `collect_tracks`. Chaque morceau lancé est signalé à YouTube (`report_play` : `get_song_tracking_url` + `add_history_item`), une fois par entrée de file.
- **Déconnexion** : propre à ytmr (la session Firefox n'est pas touchée), mémorisée par le fichier `cache_dir/library/disconnected`, efface la bibliothèque en cache. La lecture garde les cookies (qualité Premium). « Se connecter » relit la session du navigateur (`Backend::reconnect`). Connexion au compte : 3 essais.
- **Accueil** : la page d'accueil de YouTube Music (`ytmr_catalog::home`, `FEmusic_home` en JSON brut + 3 suites, contexte `hl=fr` sinon les titres arrivent en anglais). « Réécouter » devient une mosaïque 4 × 4 (2 colonnes si la colonne centrale fait < 760 px) d'albums, playlists, EP et artistes (les morceaux seuls sont écartés : doublon avec l'historique) ; puis « Tes playlists » ; puis les autres rangées de YouTube (Sélection rapide, Albums pour vous, mix, nouveautés…). Un morceau d'une rangée lance la rangée à partir de lui (`Lib::PlayHome`). Cache `home.json`, 15 min (3 min après une écoute). Mesuré : 12 rangées.
- **Navigation** : une page à charger (album, artiste, playlist hors cache) n'est affichée qu'une fois prête (`pending_route`, barre animée par-dessus la page) ; une playlist dès sa 1re page. Recherche : résultats montrés d'un coup quand toutes leurs pochettes sont là (échecs comptés comme finis, garde-fou 6 s), requête normalisée (espaces).
- **Teintes** : tout suit `accent` (morceau en cours : boutons, cœurs, survol des lignes, titres) ; seul le dégradé en haut d'une page utilise `page_accent` (sa pochette, neutre en attendant). Fondu de 0,6 s (`Fade`, abonnement `frames` seulement pendant le fondu).
- **Mémoire des pochettes** : 64 Mo décodés au plus (`COVER_BUDGET`). Seules les pochettes `cover_evictable` (lignes des listes virtualisées, préchargement) sont libérées, les moins récentes d'abord ; tout ce qui est demandé par `want_cover` (profil, cartes, grandes pochettes) reste — libérer ces dernières faisait disparaître la photo de profil et les cartes de l'accueil. Préchargement de fond (`cover_backlog`, 3 à la fois) : toutes les vignettes de la playlist ouverte, de la bibliothèque, de l'historique.
- **Ne jamais mettre une liste dans un message de vue** : l'en-tête de playlist faisait `Message::Play(tracks.to_vec())` à chaque image → `Lib::PlayAt(0)`, `Lib::PlayShuffled`, `Message::PlayAlbum`.
- **Radio** : réactiver l'autoplay remet la graine à zéro ; si une radio n'apporte que des doublons, nouvelle graine parmi les 30 derniers titres (3 essais). En aléatoire, ses titres sont répartis dans la suite (`Queue::append_mixed`), le prochain (préchargé) ne bouge pas.
- **Plein écran** : la vue « En cours » n'a ni barres latérales ni boutons ; la barre de lecture n'apparaît que 2,5 s après un mouvement de souris ; clic sur la pochette ou Échap pour revenir ; F11 bascule la fenêtre en plein écran. Plus de sigle de qualité.
- **File d'attente** (`queue.rs`) : historique (caché, pour « précédent ») + courant + suite, et un **contexte** (liste d'origine + ajouts). Lancer un titre au milieu d'une playlist met la fin puis le début dans la suite (rien n'est « joué »). Activer/couper l'aléatoire **reconstruit** la suite depuis tout le contexte. La file publiée (`QueueSnapshot`) commence au morceau en cours (`current = Some(0)`). Sans historique, « précédent » = titre d'avant dans le contexte (`prepare_previous`). Session : historique + suite + état aléatoire (`Queue::restore`).
- **Bibliothèque** (`collection.rs`) : onglets Playlists / Albums et EP / Artistes, tri (récents, A → Z, auteur / abonnés), caches `albums.json` / `artists.json` (30 min). Like : album = `rate_playlist` sur sa playlist audio `OLAK5uy…` (`Album::audio_playlist_id`, cherchée via le catalogue depuis une carte), playlist d'un autre = `rate_playlist`, artiste = `subscribe_artist` / `unsubscribe_artists`. Mesuré : 22 albums, 25 artistes.
- **Barre de titre maison** (`chrome.rs`, `decorations: false`), même fond que les barres latérales + filet `LINE` : glisser = `window::drag`, double-clic / bouton = `toggle_maximize`, bords invisibles de 5 px = `drag_resize` (masqués quand la fenêtre est agrandie ou en plein écran). Cachée en plein écran et dans la vue « En cours » tant que la souris ne bouge pas.
- **Glisser-déposer dans la file** : les lignes de la file ne sont pas des boutons (un bouton garde le clic) mais des `mouse_area` : appui = `QueueDrag::Press`, survol pendant l'appui = `Over`, relâchement (écouté partout via `event::listen_with`) = `Release` → clic bref : `JumpTo`, sinon `Command::Move` (affiché tout de suite, le core confirme). Rien ne passe devant le morceau en cours.
- **Textes longs** : iced ne coupe pas avec « … » → `views::ellipsize` + `Wrapping::None` + `clip`, le cœur reste à côté du titre. Un seul cœur (16 px) partout.
- **Aléatoire depuis une page** : `Command::PlayShuffled` (`Queue::replace_shuffled`) mélange toute la liste et part du premier. Avant, `SetShuffle` + `PlayTracks` depuis un index tiré au hasard ne mélangeait que les titres *après* lui (parti du dernier : file vide).
- **Fin de mise à jour d'une playlist** : l'adresse de sa pochette peut changer → `finish_playlist` redemande la grande pochette et la teinte (garde l'ancienne adresse si YouTube n'en donne pas).
- **Encodage** : ne jamais réécrire un fichier source avec `Get-Content`/`Set-Content` de PowerShell 5.1 (lu en ANSI → accents doublement encodés, « Ã© »). Passer par Python ou l'outil d'édition.
- **Pochettes qui ne s'affichaient pas** (2026-10-29) : avec les cartes de l'accueil gardées en mémoire (320 px = 410 Ko chacune), le budget était dépassé et chaque vignette de playlist tout juste chargée était libérée aussitôt (seule celle du morceau en cours restait). D'où `CARD` = 200 px, budget 96 Mo, et `visible_covers` : une pochette affichée n'est jamais libérée. Un échec de chargement retire l'entrée (une prochaine demande retente).
- **Accueil figé pendant la session** (demande de l'utilisateur) : le cache s'affiche au lancement, la version fraîche n'est qu'enregistrée (visible au prochain lancement).
- **Mode focus** : dans la vue « En cours », rien n'apparaît au mouvement de la souris ; un clic affiche / masque les contrôles. Bulles d'aide : le nom seul.
- **Paroles** (`ytmr_catalog::lyrics`) : LRCLIB `/api/get` + `/api/search` + YouTube Music (`get_lyrics`) en parallèle ; classement : synchronisées à ±3 s de la durée > texte simple > synchronisées d'une autre version. Affichées dans la vue « En cours » (bouton micro de la barre du bas), ligne chantée en évidence, clic = aller à ce moment. Mesuré : 1,4 s (Get Lucky, LRCLIB).
- **Menu clic droit** (`context.rs`) : `mouse_area::on_right_press` dit la cible ; la position vient de `PointCatcher`, posé autour de toute la fenêtre (les listes défilantes décalent la position vue par leurs lignes). Liens copiés : `watch?v=`, `browse/MPRE…`, `channel/UC…`, `playlist?list=`.
- **Suggestions** : `Catalog::suggestions` (`get_search_suggestions`), 140 ms après la frappe, sous le champ de recherche.
- **Historique** : `Route::History`, ouvert par le profil (bas de la barre latérale) ou les Paramètres.
- **Barre des tâches Windows** (`ytmr_os::taskbar`) : `ITaskbarList3::ThumbBarAddButtons` + sous-classement de la fenêtre (`SetWindowSubclass`, sur le fil de la fenêtre : appelé depuis `native_handle`) ; le fil de `ytmr-os` poste `WM_APP+0x31` pour changer lecture/pause. Icônes dessinées en code (20 px). Reposées sur `TaskbarButtonCreated`.
- **Notifications** (`notify-rust`) : à chaque nouveau morceau si la fenêtre n'a pas le focus (réglable, `ui.json`). Windows : AUMID `ytmr.player` déclaré dans `HKCU\Software\Classes\AppUserModelId` (sinon la bulle porte le nom de PowerShell).
- **Fondu enchaîné** : mpv ne lit qu'un flux, donc pas de vrai chevauchement : fondu de sortie sur les X dernières secondes puis d'entrée sur le suivant, seulement aux enchaînements automatiques. Filtre étiqueté `@ytmrfade:lavfi=[volume=1]` en fin de chaîne `af`, gain piloté à chaque position (250 ms) par `af-command ytmrfade volume <g> volume` (la cible `volume` est obligatoire).
- Pas encore : `/api/v1/like` (501) pour le CYD.

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
| 4 — GUI native iced | validée sous Windows (recherche, album, artiste, file, lecture) |
| 5 — Firmware CYD | à faire |
| 6 — Authentification et bibliothèque (playlists, likes) | faite sous Windows (lecture vérifiée sur un vrai compte ; modifications à confirmer par l'utilisateur) |
| 7 — Intégration OS | touches média + overlay Windows (SMTC) branchés (`ytmr-os`, souvlaki sur un fil dédié, HWND fourni par la GUI) ; MPRIS (zbus) à vérifier sous CachyOS ; pas de notification ni d'icône de tray |
| 8 — Packaging | à faire |
