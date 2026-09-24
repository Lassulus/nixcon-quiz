//! Break slides for the spectator screen: images and PDFs from a directory
//! that is scanned again before every change of slide, so files can be added,
//! replaced or removed while the quiz runs. Every page of a PDF is a slide of
//! its own, rendered to PNG with poppler's `pdftoppm`.
//!
//! Slides are shown in file name order; prefix names with numbers to choose
//! it.

use axum::body::Bytes;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::SystemTime,
};

/// Width of rendered PDF pages, or height for portrait ones: sharp on a
/// 1080p stream even at full size.
const RENDER_PIXELS: &str = "1920";

pub struct Slide {
    /// Name under `/slides/`, derived from the content, so browsers may
    /// cache it forever.
    pub name: String,
    /// File name without extension.
    pub alt: String,
    /// File name and page: the order slides are shown in.
    order: (String, u32),
    pub content_type: &'static str,
    pub body: Bytes,
}

enum Kind {
    Pdf,
    Image(&'static str),
}

fn kind(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => Kind::Pdf,
        "png" => Kind::Image("image/png"),
        "jpg" | "jpeg" => Kind::Image("image/jpeg"),
        "webp" => Kind::Image("image/webp"),
        _ => return None,
    })
}

/// Modification time and size: a file whose stamp is unchanged is not read
/// or rendered again.
type Stamp = (Option<SystemTime>, u64);

pub struct Library {
    dir: PathBuf,
    /// The last scan, by path. Files that failed to load are kept with no
    /// slides, so their error is reported once rather than on every scan.
    files: HashMap<PathBuf, (Stamp, Vec<Arc<Slide>>)>,
    /// Why the directory couldn't be read last time, to report it once.
    error: Option<String>,
}

impl Library {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            files: HashMap::new(),
            error: None,
        }
    }

    /// Every slide in the directory now, in order. Blocks while new or
    /// changed PDFs are rendered.
    pub fn scan(&mut self) -> Vec<Arc<Slide>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => {
                self.error = None;
                entries
            }
            Err(e) => {
                let error = format!("slides: {}: {e}", self.dir.display());
                if self.error.as_ref() != Some(&error) {
                    eprintln!("{error}");
                    self.error = Some(error);
                }
                self.files.clear();
                return Vec::new();
            }
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            // Dotfiles include rsync's temporary copies of files on their way.
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path())
            .collect();
        paths.sort();
        let mut old = std::mem::take(&mut self.files);
        let mut slides = Vec::new();
        for path in paths {
            let Some(kind) = kind(&path) else { continue };
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let stamp = (meta.modified().ok(), meta.len());
            let loaded = match old.remove(&path) {
                Some((seen, loaded)) if seen == stamp => loaded,
                _ => load(&path, kind).unwrap_or_else(|e| {
                    eprintln!("slides: {}: {e}", path.display());
                    Vec::new()
                }),
            };
            slides.extend(loaded.iter().cloned());
            self.files.insert(path, (stamp, loaded));
        }
        slides.sort_by(|a, b| a.order.cmp(&b.order));
        slides
    }
}

/// The slide to show after `current`: the next one in order, so slides added
/// or removed meanwhile don't restart the rotation.
pub fn next(slides: &[Arc<Slide>], current: Option<&Slide>) -> Option<Arc<Slide>> {
    current
        .and_then(|c| slides.iter().find(|s| s.order > c.order))
        .or(slides.first())
        .cloned()
}

fn load(path: &Path, kind: Kind) -> Result<Vec<Arc<Slide>>, String> {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let alt = path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let slide = |page: u32, content_type: &'static str, ext: &str, body: Vec<u8>| {
        Arc::new(Slide {
            name: format!("{:016x}.{ext}", fnv1a(&body)),
            alt: alt.clone(),
            order: (file.clone(), page),
            content_type,
            body: body.into(),
        })
    };
    match kind {
        Kind::Image(content_type) => {
            let body = fs::read(path).map_err(|e| e.to_string())?;
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            Ok(vec![slide(
                1,
                content_type,
                &ext.to_ascii_lowercase(),
                body,
            )])
        }
        Kind::Pdf => (1..=pdf_pages(path)?)
            .map(|page| Ok(slide(page, "image/png", "png", render(path, page)?)))
            .collect(),
    }
}

fn run(command: &mut Command) -> Result<Vec<u8>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|e| format!("running {program}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{program} failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(output.stdout)
}

fn pdf_pages(path: &Path) -> Result<u32, String> {
    let info = run(Command::new("pdfinfo").arg(path))?;
    String::from_utf8_lossy(&info)
        .lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| "pdfinfo reports no page count".to_string())
}

fn render(path: &Path, page: u32) -> Result<Vec<u8>, String> {
    let page = page.to_string();
    run(Command::new("pdftoppm")
        .args(["-png", "-scale-to", RENDER_PIXELS, "-singlefile"])
        .args(["-f", &page, "-l", &page])
        .arg(path))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slide(file: &str, page: u32) -> Arc<Slide> {
        Arc::new(Slide {
            name: format!("{file}-{page}"),
            alt: String::new(),
            order: (file.to_string(), page),
            content_type: "image/png",
            body: Bytes::new(),
        })
    }

    #[test]
    fn rotation_continues_in_order_when_slides_come_and_go() {
        let a = slide("a.png", 1);
        let b2 = slide("b.pdf", 2);
        let b10 = slide("b.pdf", 10);
        let c = slide("c.png", 1);
        let name = |s: Option<Arc<Slide>>| s.map(|s| s.name.clone());

        assert_eq!(
            name(next(&[a.clone(), c.clone()], None)),
            Some("a.png-1".into())
        );
        // Page 10 comes after page 2, not before it.
        let all = [a.clone(), b2.clone(), b10.clone(), c.clone()];
        assert_eq!(name(next(&all, Some(&b2))), Some("b.pdf-10".into()));
        assert_eq!(name(next(&all, Some(&c))), Some("a.png-1".into()), "wraps");
        // The current slide was removed: carry on with the one after it.
        assert_eq!(
            name(next(&[a.clone(), c.clone()], Some(&b2))),
            Some("c.png-1".into())
        );
        assert_eq!(name(next(&[], Some(&a))), None);
    }
}
