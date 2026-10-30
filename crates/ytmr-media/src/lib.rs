//! Jaquettes : téléchargement, recadrage, redimensionnement, cache disque.
//!
//! En clair : on récupère l'image une fois en grand, on la garde sur le disque,
//! puis on fabrique à la demande la taille et le format exacts voulus par
//! l'écran qui la demande (petite vignette pour une liste, grande image pour
//! le lecteur, pixels RGB565 bruts pour l'ESP32). Chaque variante est gardée
//! aussi, donc la deuxième demande est une simple lecture de fichier.
//!
//! Une jaquette est désignée par un `cover_id` : une empreinte courte de son
//! URL d'origine. On donne cet identifiant aux clients plutôt que l'URL.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, RgbImage};
use thiserror::Error;
use tokio::sync::Semaphore;

/// Tailles téléchargées depuis YouTube (les URL Google acceptent une taille au
/// choix) : une petite pour les vignettes, une grande pour les pochettes en
/// grand. Télécharger 1 200 px pour une vignette de 48 px coûtait ~150 Ko et un
/// décodage lourd par ligne de liste.
const SOURCE_LARGE: u32 = 1200;
const SOURCE_SMALL: u32 = 240;
/// Au-delà, on prend la grande source.
const SMALL_MAX: u32 = 200;
/// Décodages / redimensionnements menés en même temps (le reste attend) :
/// l'interface garde du processeur pour rester fluide.
const MAX_CONCURRENT_DECODES: usize = 2;
pub const MAX_JPEG_SIZE: u32 = 1200;
/// L'écran du CYD fait 320 px de large.
pub const MAX_RGB565_SIZE: u32 = 320;
pub const MIN_SIZE: u32 = 16;
const JPEG_QUALITY: u8 = 88;
const MAX_CONCURRENT_DOWNLOADS: usize = 4;
/// Au-delà, le cache disque est purgé au démarrage (les plus anciens d'abord)…
const CACHE_LIMIT: u64 = 250 * 1024 * 1024;
/// … jusqu'à redescendre sous ce seuil.
const CACHE_TARGET: u64 = 200 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("jaquette inconnue")]
    UnknownCover,
    #[error("taille {0} hors bornes")]
    InvalidSize(u32),
    #[error("téléchargement : {0}")]
    Download(#[from] reqwest::Error),
    #[error("image illisible : {0}")]
    Decode(#[from] image::ImageError),
    #[error("cache disque : {0}")]
    Io(#[from] std::io::Error),
    #[error("mode hors ligne")]
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverFormat {
    Jpeg,
    /// Octets bruts RGB565 little-endian, `size * size * 2` octets.
    Rgb565,
}

impl CoverFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Rgb565 => "rgb565",
        }
    }

    pub fn max_size(self) -> u32 {
        match self {
            Self::Jpeg => MAX_JPEG_SIZE,
            Self::Rgb565 => MAX_RGB565_SIZE,
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Rgb565 => "application/octet-stream",
        }
    }
}

/// Empreinte stable d'une URL de jaquette (16 caractères hexadécimaux).
///
/// FNV-1a 64 bits : contrairement à `DefaultHasher`, elle ne change pas d'une
/// version de Rust à l'autre, donc le cache disque reste valable.
pub fn cover_id(url: &str) -> String {
    let hash = normalize_url(url)
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("{hash:016x}")
}

/// Une même image Google existe à toutes les tailles (`=w120-h120…`) : on
/// ignore ce suffixe pour qu'elles partagent le même identifiant. Les
/// vignettes vidéo (`i.ytimg.com/vi/…`) portent des paramètres signés
/// (`?sqp=…&rs=…`) qui changent à chaque réponse de YouTube pour la même
/// image : ignorés aussi (sinon toute une playlist se rechargeait à chaque
/// mise à jour).
pub fn normalize_url(url: &str) -> &str {
    if is_google_image(url)
        && let Some((base, _)) = url.rsplit_once('=')
    {
        return base;
    }
    if url.contains("i.ytimg.com/")
        && let Some((base, _)) = url.split_once('?')
    {
        return base;
    }
    url
}

fn is_google_image(url: &str) -> bool {
    url.contains(".googleusercontent.com/") || url.contains(".ggpht.com/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Small,
    Large,
}

impl Tier {
    fn for_size(size: u32) -> Self {
        if size <= SMALL_MAX {
            Self::Small
        } else {
            Self::Large
        }
    }

    fn file(self, id: &str) -> String {
        match self {
            Self::Small => format!("{id}_s.img"),
            Self::Large => format!("{id}.img"),
        }
    }
}

/// URL à télécharger pour une taille de source donnée.
fn source_url(url: &str, tier: Tier) -> String {
    if is_google_image(url) {
        let side = match tier {
            Tier::Small => SOURCE_SMALL,
            Tier::Large => SOURCE_LARGE,
        };
        return format!("{}=w{side}-h{side}-l90-rj", normalize_url(url));
    }
    // Vignettes vidéo i.ytimg.com : `mqdefault` (320×180) suffit pour une
    // vignette ; `hqdefault` (480×360) existe toujours, contrairement à « maxres ».
    if url.contains("i.ytimg.com/vi/")
        && let Some((base, _)) = url.rsplit_once('/')
    {
        let name = match tier {
            Tier::Small => "mqdefault.jpg",
            Tier::Large => "hqdefault.jpg",
        };
        return format!("{base}/{name}");
    }
    url.to_owned()
}

pub struct Covers {
    dir: PathBuf,
    http: reqwest::Client,
    urls: Mutex<HashMap<String, String>>,
    downloads: Semaphore,
    decodes: Semaphore,
    /// Mode hors ligne : seules les jaquettes déjà sur le disque.
    offline: AtomicBool,
}

impl Covers {
    /// `dir` : dossier de cache (créé au besoin).
    pub fn new(dir: PathBuf) -> Result<Self, MediaError> {
        std::fs::create_dir_all(dir.join("src"))?;
        prune(&dir, CACHE_LIMIT, CACHE_TARGET);
        Ok(Self {
            dir,
            http: reqwest::Client::new(),
            urls: Mutex::default(),
            downloads: Semaphore::new(MAX_CONCURRENT_DOWNLOADS),
            decodes: Semaphore::new(MAX_CONCURRENT_DECODES),
            offline: AtomicBool::new(false),
        })
    }

    /// Mode hors ligne : plus aucun téléchargement.
    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::Relaxed);
    }

    /// Mémorise l'URL d'une jaquette et rend son identifiant.
    pub fn register(&self, url: &str) -> String {
        let id = cover_id(url);
        lock(&self.urls)
            .entry(id.clone())
            .or_insert_with(|| url.to_owned());
        id
    }

    /// Jaquette `id` en `size`×`size` au format demandé.
    pub async fn get(&self, id: &str, size: u32, format: CoverFormat) -> Result<Bytes, MediaError> {
        if !(MIN_SIZE..=format.max_size()).contains(&size) {
            return Err(MediaError::InvalidSize(size));
        }
        if !is_valid_id(id) {
            return Err(MediaError::UnknownCover);
        }
        let variant = self.dir.join(format!("{id}_{size}.{}", format.extension()));
        if let Ok(data) = tokio::fs::read(&variant).await {
            return Ok(data.into());
        }

        let source = self.source(id, Tier::for_size(size)).await?;
        let data = self.blocking(move || render(&source, size, format)).await?;
        write_atomic(&variant, &data).await?;
        Ok(data.into())
    }

    /// Fichier JPEG de la jaquette en `size` px (créé au besoin), pour les
    /// programmes qui veulent un chemin (notifications du système).
    pub async fn jpeg_file(&self, id: &str, size: u32) -> Result<PathBuf, MediaError> {
        self.get(id, size, CoverFormat::Jpeg).await?;
        Ok(self.dir.join(format!("{id}_{size}.jpg")))
    }

    /// Jaquette décodée en pixels RGBA (`size * size * 4` octets), prête à
    /// afficher sans décodage dans le fil de l'interface. Rendue directement
    /// depuis la source (sans JPEG intermédiaire) ; la variante JPEG est
    /// gardée sur disque pour la fois suivante.
    pub async fn get_rgba(&self, id: &str, size: u32) -> Result<Vec<u8>, MediaError> {
        if !(MIN_SIZE..=MAX_JPEG_SIZE).contains(&size) {
            return Err(MediaError::InvalidSize(size));
        }
        if !is_valid_id(id) {
            return Err(MediaError::UnknownCover);
        }
        let variant = self.dir.join(format!("{id}_{size}.jpg"));
        if let Ok(jpeg) = tokio::fs::read(&variant).await {
            return self
                .blocking(move || Ok(image::load_from_memory(&jpeg)?.to_rgba8().into_raw()))
                .await;
        }
        let source = self.source(id, Tier::for_size(size)).await?;
        let (rgba, jpeg) = self
            .blocking(move || {
                let rgb = scaled(&source, size)?;
                let jpeg = encode_jpeg(&rgb)?;
                let rgba = image::DynamicImage::ImageRgb8(rgb).to_rgba8().into_raw();
                Ok((rgba, jpeg))
            })
            .await?;
        write_atomic(&variant, &jpeg).await?;
        Ok(rgba)
    }

    /// Couleur dominante (vive) de la jaquette, pour teinter une interface.
    pub async fn accent(&self, id: &str) -> Result<[u8; 3], MediaError> {
        let source = self.source(id, Tier::Small).await?;
        self.blocking(move || dominant_color(&source)).await
    }

    /// Travail processeur hors du fil courant, au plus `MAX_CONCURRENT_DECODES` à la fois.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, MediaError> + Send + 'static,
    ) -> Result<T, MediaError> {
        let _slot = self
            .decodes
            .acquire()
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?
    }

    /// Image d'origine, téléchargée au premier besoin et gardée sur disque.
    async fn source(&self, id: &str, tier: Tier) -> Result<Vec<u8>, MediaError> {
        let path = self.dir.join("src").join(tier.file(id));
        if let Ok(data) = tokio::fs::read(&path).await {
            return Ok(data);
        }
        // Hors ligne : l'autre taille, si elle est là, vaut mieux que rien.
        if self.offline.load(Ordering::Relaxed) {
            let other = match tier {
                Tier::Small => Tier::Large,
                Tier::Large => Tier::Small,
            };
            return tokio::fs::read(self.dir.join("src").join(other.file(id)))
                .await
                .map_err(|_| MediaError::Offline);
        }
        let url = lock(&self.urls)
            .get(id)
            .cloned()
            .ok_or(MediaError::UnknownCover)?;
        let _slot = self
            .downloads
            .acquire()
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // Un autre appel l'a peut-être téléchargée pendant qu'on attendait.
        if let Ok(data) = tokio::fs::read(&path).await {
            return Ok(data);
        }
        if self.offline.load(Ordering::Relaxed) {
            return Err(MediaError::Offline);
        }
        let response = self
            .http
            .get(source_url(&url, tier))
            .send()
            .await?
            .error_for_status()?;
        let data = response.bytes().await?.to_vec();
        // Vérifie que c'est bien une image avant de la garder.
        image::load_from_memory(&data)?;
        write_atomic(&path, &data).await?;
        tracing::debug!(id, bytes = data.len(), "jaquette téléchargée");
        Ok(data)
    }
}

/// Supprime les fichiers les moins récemment écrits tant que le cache dépasse `target`.
fn prune(dir: &Path, limit: u64, target: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> =
        [dir.to_path_buf(), dir.join("src")]
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                meta.is_file().then(|| {
                    (
                        meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                        meta.len(),
                        entry.path(),
                    )
                })
            })
            .collect();
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= limit {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut removed = 0usize;
    for (_, len, path) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            removed += 1;
        }
    }
    tracing::info!(
        removed,
        remaining_mb = total / (1024 * 1024),
        "cache de jaquettes purgé"
    );
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn is_valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn write_atomic(path: &Path, data: &[u8]) -> Result<(), MediaError> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    tokio::fs::write(&tmp, data).await?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        // Une autre tâche a écrit le même fichier : le résultat est identique.
        if !path.exists() {
            return Err(e.into());
        }
    }
    Ok(())
}

/// Carré central : les vignettes vidéo de YouTube Music sont des pochettes
/// carrées entourées de bandes noires.
fn square(image: &DynamicImage) -> DynamicImage {
    let (w, h) = image.dimensions();
    let side = w.min(h);
    image.crop_imm((w - side) / 2, (h - side) / 2, side, side)
}

/// Carré central redimensionné. CatmullRom : quasiment aussi net que Lanczos
/// en réduction, nettement moins coûteux.
fn scaled(source: &[u8], size: u32) -> Result<RgbImage, MediaError> {
    let image = square(&image::load_from_memory(source)?);
    Ok(image
        .resize_exact(size, size, FilterType::CatmullRom)
        .to_rgb8())
}

fn encode_jpeg(rgb: &RgbImage) -> Result<Vec<u8>, MediaError> {
    let mut out = Cursor::new(Vec::new());
    rgb.write_with_encoder(JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY))?;
    Ok(out.into_inner())
}

fn render(source: &[u8], size: u32, format: CoverFormat) -> Result<Vec<u8>, MediaError> {
    let rgb = scaled(source, size)?;
    match format {
        CoverFormat::Jpeg => encode_jpeg(&rgb),
        CoverFormat::Rgb565 => Ok(to_rgb565_le(&rgb)),
    }
}

/// RRRRRGGG GGGBBBBB, octet de poids faible en premier.
pub fn to_rgb565_le(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::with_capacity((image.width() * image.height() * 2) as usize);
    for pixel in image.pixels() {
        let [r, g, b] = pixel.0;
        let value = (u16::from(r) >> 3) << 11 | (u16::from(g) >> 2) << 5 | u16::from(b) >> 3;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Couleur la plus représentée parmi les pixels assez saturés et ni trop
/// sombres ni trop clairs ; gris neutre si la pochette est monochrome.
fn dominant_color(source: &[u8]) -> Result<[u8; 3], MediaError> {
    let small = square(&image::load_from_memory(source)?)
        .resize_exact(48, 48, FilterType::Triangle)
        .to_rgb8();
    // Histogramme sur 4 bits par canal, pondéré par la saturation.
    let mut buckets: HashMap<u16, (f32, [f32; 3])> = HashMap::new();
    for pixel in small.pixels() {
        let [r, g, b] = pixel.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let lightness = (u16::from(max) + u16::from(min)) / 2;
        if !(40..=220).contains(&lightness) {
            continue;
        }
        let saturation = f32::from(max - min) / f32::from(max.max(1));
        if saturation < 0.25 {
            continue;
        }
        let key = u16::from(r >> 4) << 8 | u16::from(g >> 4) << 4 | u16::from(b >> 4);
        let entry = buckets.entry(key).or_insert((0.0, [0.0; 3]));
        entry.0 += saturation;
        entry.1[0] += f32::from(r) * saturation;
        entry.1[1] += f32::from(g) * saturation;
        entry.1[2] += f32::from(b) * saturation;
    }
    let best = buckets
        .values()
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(weight, sum)| sum.map(|c| (c / weight).round().clamp(0.0, 255.0) as u8));
    Ok(best.unwrap_or([128, 128, 128]))
}

#[cfg(test)]
mod tests {
    #[test]
    fn signed_video_thumbnails_share_one_id() {
        let a = "https://i.ytimg.com/vi/abc/hqdefault.jpg?sqp=AAA&rs=111";
        let b = "https://i.ytimg.com/vi/abc/hqdefault.jpg?sqp=BBB&rs=222";
        assert_eq!(cover_id(a), cover_id(b));
        assert_ne!(
            cover_id(a),
            cover_id("https://i.ytimg.com/vi/xyz/hqdefault.jpg")
        );
    }

    use image::Rgb;

    use super::*;

    #[test]
    fn rgb565_is_little_endian() {
        let mut image = RgbImage::new(2, 1);
        image.put_pixel(0, 0, Rgb([255, 0, 0]));
        image.put_pixel(1, 0, Rgb([0, 0, 255]));
        // Rouge = 0xF800, bleu = 0x001F.
        assert_eq!(to_rgb565_le(&image), [0x00, 0xF8, 0x1F, 0x00]);
    }

    #[test]
    fn google_sizes_share_one_id() {
        let a = "https://lh3.googleusercontent.com/abc=w60-h60-l90-rj";
        let b = "https://lh3.googleusercontent.com/abc=w544-h544-l90-rj";
        assert_eq!(cover_id(a), cover_id(b));
        assert_eq!(cover_id(a).len(), 16);
        assert_eq!(
            source_url(a, Tier::Large),
            "https://lh3.googleusercontent.com/abc=w1200-h1200-l90-rj"
        );
        assert_eq!(
            source_url(a, Tier::Small),
            "https://lh3.googleusercontent.com/abc=w240-h240-l90-rj"
        );
    }

    #[test]
    fn video_thumbnails_use_hqdefault() {
        assert_eq!(
            source_url(
                "https://i.ytimg.com/vi/Jb6gcoR266U/sddefault.jpg?sqp=x",
                Tier::Large
            ),
            "https://i.ytimg.com/vi/Jb6gcoR266U/hqdefault.jpg"
        );
    }

    fn test_png(w: u32, h: u32, color: [u8; 3]) -> Vec<u8> {
        let image = RgbImage::from_pixel(w, h, Rgb(color));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn renders_square_variants() {
        let png = test_png(160, 90, [200, 30, 30]);
        let jpeg = render(&png, 64, CoverFormat::Jpeg).unwrap();
        assert_eq!(
            image::load_from_memory(&jpeg).unwrap().dimensions(),
            (64, 64)
        );
        let raw = render(&png, 120, CoverFormat::Rgb565).unwrap();
        assert_eq!(raw.len(), 120 * 120 * 2);
    }

    #[test]
    fn dominant_color_prefers_saturated_pixels() {
        let mut image = RgbImage::from_pixel(100, 100, Rgb([20, 20, 20]));
        for x in 0..40 {
            for y in 0..100 {
                image.put_pixel(x, y, Rgb([30, 120, 220]));
            }
        }
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let [r, g, b] = dominant_color(&png.into_inner()).unwrap();
        assert!(b > 180 && r < 60, "bleu attendu, obtenu {r},{g},{b}");
    }

    #[test]
    fn prune_removes_oldest_files_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![0u8; 100]).unwrap();
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(time)
                .unwrap();
        }
        prune(dir.path(), 350, 200);
        let left: Vec<_> = ["a", "b", "c", "d"]
            .iter()
            .filter(|n| dir.path().join(n).exists())
            .collect();
        assert_eq!(left, [&"c", &"d"]);
    }

    #[tokio::test]
    async fn rejects_bad_input_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let covers = Covers::new(dir.path().to_path_buf()).unwrap();
        assert!(matches!(
            covers
                .get("0123456789abcdef", 999, CoverFormat::Rgb565)
                .await,
            Err(MediaError::InvalidSize(999))
        ));
        assert!(matches!(
            covers.get("../../etc/passwd", 64, CoverFormat::Jpeg).await,
            Err(MediaError::UnknownCover)
        ));
        assert!(matches!(
            covers.get("0123456789abcdef", 64, CoverFormat::Jpeg).await,
            Err(MediaError::UnknownCover)
        ));
    }
}
