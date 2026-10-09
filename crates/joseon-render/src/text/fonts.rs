//! Fonts for the overlay: the system UI font for readouts, the system
//! monospaced font for axis numbers.
//!
//! Port source: `Fonts` in `Sources/JoseonRender/TextOverlay.swift`, where Core
//! Text does this work. Here `ab_glyph` reads the font files of the system:
//! Segoe UI and Cascadia Mono (or Consolas) on Windows, San Francisco on a Mac.
//! No font is in the repository.
//!
//! Sizes are in points. A glyph is rasterized at `size x scale` pixels, at one
//! of four sub-pixel positions, and kept in a cache.

use ab_glyph::{Font as _, FontVec, Glyph, GlyphId, PxScale, VariableFont};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Light,
    Regular,
    Medium,
    Semibold,
    Bold,
}

impl Weight {
    /// The value on the `wght` axis of a variable font.
    fn axis_value(self) -> f32 {
        match self {
            Self::Light => 300.0,
            Self::Regular => 400.0,
            Self::Medium => 500.0,
            Self::Semibold => 600.0,
            Self::Bold => 700.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Family {
    Ui,
    Mono,
}

/// One font file at one weight.
struct Face {
    id: u32,
    font: FontVec,
    units_per_em: f32,
    /// All as a part of the em.
    ascent: f32,
    descent: f32,
    cap_height: f32,
    /// The scale `ab_glyph` wants for one pixel per em.
    px_scale_per_em: f32,
    /// The widest digit, as a part of the em. Digits are set on this advance, so numbers do not move.
    digit_advance: f32,
}

impl Face {
    fn load(id: u32, path: &str, index: u32, weight: Option<f32>, optical_size: f32) -> Option<Face> {
        let data = std::fs::read(path).ok()?;
        let mut font = FontVec::try_from_vec_and_index(data, index).ok()?;
        if let Some(weight) = weight {
            font.set_variation(b"wght", weight);
            // The optical size, where a font has that axis: wider letters for small text, the display cut for
            // large numerals. (With no value the font gives its display cut at every size.)
            font.set_variation(b"opsz", optical_size);
        }
        let units_per_em = font.units_per_em()?;
        let cap_height = font
            .outline(font.glyph_id('H'))
            .map(|outline| (outline.bounds.max.y - outline.bounds.min.y).abs() / units_per_em)
            .filter(|h| *h > 0.0)
            .unwrap_or(0.7);
        let digit_advance = ('0'..='9')
            .map(|c| font.h_advance_unscaled(font.glyph_id(c)) / units_per_em)
            .fold(0.0f32, f32::max);
        Some(Face {
            id,
            units_per_em,
            ascent: font.ascent_unscaled() / units_per_em,
            descent: -font.descent_unscaled() / units_per_em,
            cap_height,
            px_scale_per_em: font.height_unscaled() / units_per_em,
            digit_advance,
            font,
        })
    }

    fn glyph(&self, c: char) -> Option<GlyphId> {
        let id = self.font.glyph_id(c);
        (id.0 != 0).then_some(id)
    }

    fn advance(&self, id: GlyphId) -> f32 {
        self.font.h_advance_unscaled(id) / self.units_per_em
    }
}

/// A family at one weight, with the faces that give the glyphs it lacks (♯, −, …).
struct FaceSet {
    primary: Option<Arc<Face>>,
    fallbacks: Vec<Arc<Face>>,
    tabular_digits: bool,
}

/// A font at a size in points. Cheap to copy.
#[derive(Clone)]
pub struct Font {
    set: Arc<FaceSet>,
    /// A number that is the same for the same family and weight.
    set_id: u32,
    pub size: f32,
}

impl PartialEq for Font {
    fn eq(&self, other: &Self) -> bool {
        self.set_id == other.set_id && self.size == other.size
    }
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Font({}, {} pt)", self.set_id, self.size)
    }
}

/// One glyph of a laid-out line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    face: u32,
    glyph: u16,
    /// From the start of the line, points.
    pub x: f32,
}

/// A line of text, laid out once and kept.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub glyphs: Vec<PlacedGlyph>,
    /// Typographic width, points.
    pub width: f32,
    /// Never used again for another line: the text layer compares lines by this.
    pub id: u64,
}

/// The face sets made so far, each with its number.
type FaceSets = HashMap<(Family, Weight, u32), (u32, Arc<FaceSet>)>;

struct Book {
    sets: Mutex<FaceSets>,
    faces: Mutex<Vec<Arc<Face>>>,
    symbols: Vec<Arc<Face>>,
}

fn book() -> &'static Book {
    static BOOK: OnceLock<Book> = OnceLock::new();
    BOOK.get_or_init(|| {
        let mut faces = Vec::new();
        let mut symbols = Vec::new();
        for (path, index) in symbol_candidates() {
            if let Some(face) = Face::load(faces.len() as u32, &path, index, None, 17.0) {
                let face = Arc::new(face);
                faces.push(face.clone());
                symbols.push(face);
            }
        }
        Book {
            sets: Mutex::new(HashMap::new()),
            faces: Mutex::new(faces),
            symbols,
        }
    })
}

/// Where the font files of the system are: path, index in a collection, and
/// whether the file is a variable font with a weight axis.
fn candidates(family: Family, weight: Weight) -> Vec<(String, u32, bool)> {
    let mut out = Vec::new();
    let bold = matches!(weight, Weight::Semibold | Weight::Bold);
    if cfg!(target_os = "windows") {
        let fonts = format!(
            "{}\\Fonts\\",
            std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string())
        );
        match family {
            Family::Ui => {
                out.push((format!("{fonts}SegUIVar.ttf"), 0, true));
                let file = match weight {
                    Weight::Light => "segoeuil.ttf",
                    Weight::Regular => "segoeui.ttf",
                    Weight::Medium | Weight::Semibold => "seguisb.ttf",
                    Weight::Bold => "segoeuib.ttf",
                };
                out.push((format!("{fonts}{file}"), 0, false));
                out.push((format!("{fonts}segoeui.ttf"), 0, false));
            }
            Family::Mono => {
                out.push((format!("{fonts}CascadiaMono.ttf"), 0, true));
                out.push((
                    format!("{fonts}{}", if bold { "consolab.ttf" } else { "consola.ttf" }),
                    0,
                    false,
                ));
                out.push((format!("{fonts}consola.ttf"), 0, false));
            }
        }
    } else if cfg!(target_os = "macos") {
        match family {
            Family::Ui => {
                out.push(("/System/Library/Fonts/SFNS.ttf".to_string(), 0, true));
                out.push((
                    "/System/Library/Fonts/Helvetica.ttc".to_string(),
                    u32::from(bold),
                    false,
                ));
            }
            Family::Mono => {
                out.push(("/System/Library/Fonts/SFNSMono.ttf".to_string(), 0, true));
                out.push(("/System/Library/Fonts/Menlo.ttc".to_string(), u32::from(bold), false));
            }
        }
    } else {
        let dejavu = "/usr/share/fonts/truetype/dejavu/";
        let file = match (family, bold) {
            (Family::Ui, false) => "DejaVuSans.ttf",
            (Family::Ui, true) => "DejaVuSans-Bold.ttf",
            (Family::Mono, false) => "DejaVuSansMono.ttf",
            (Family::Mono, true) => "DejaVuSansMono-Bold.ttf",
        };
        out.push((format!("{dejavu}{file}"), 0, false));
    }
    out
}

fn symbol_candidates() -> Vec<(String, u32)> {
    if cfg!(target_os = "windows") {
        let fonts = format!(
            "{}\\Fonts\\",
            std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string())
        );
        vec![(format!("{fonts}seguisym.ttf"), 0), (format!("{fonts}segoeui.ttf"), 0)]
    } else if cfg!(target_os = "macos") {
        vec![
            ("/System/Library/Fonts/Apple Symbols.ttf".to_string(), 0),
            ("/System/Library/Fonts/Helvetica.ttc".to_string(), 0),
        ]
    } else {
        vec![("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".to_string(), 0)]
    }
}

/// The optical size a point size is drawn with: one of four steps from 17 (text) to 28 (display), as the system
/// font of macOS changes its cut between those sizes. Few steps, so few copies of the font are loaded.
fn optical_size(size: f32) -> u32 {
    [17u32, 20, 24, 28]
        .into_iter()
        .min_by(|a, b| (*a as f32 - size).abs().total_cmp(&(*b as f32 - size).abs()))
        .unwrap_or(17)
}

fn font(family: Family, size: f32, weight: Weight) -> Font {
    let book = book();
    let mut sets = book.sets.lock().unwrap();
    let next_id = sets.len() as u32 + 1;
    let optical = optical_size(size);
    let (set_id, set) = sets.entry((family, weight, optical)).or_insert_with(|| {
        let mut faces = book.faces.lock().unwrap();
        let primary = candidates(family, weight)
            .into_iter()
            .find_map(|(path, index, variable)| {
                Face::load(
                    faces.len() as u32,
                    &path,
                    index,
                    variable.then_some(weight.axis_value()),
                    optical as f32,
                )
            });
        let primary = primary.map(Arc::new);
        if let Some(face) = &primary {
            faces.push(face.clone());
        }
        (
            next_id,
            Arc::new(FaceSet {
                primary,
                fallbacks: book.symbols.clone(),
                tabular_digits: true,
            }),
        )
    });
    Font {
        set: set.clone(),
        set_id: *set_id,
        size,
    }
}

/// The UI font with digits of one width.
pub fn ui(size: f32, weight: Weight) -> Font {
    font(Family::Ui, size, weight)
}

/// The monospaced font.
pub fn mono(size: f32, weight: Weight) -> Font {
    font(Family::Mono, size, weight)
}

/// False when the system has none of the fonts this module knows. Text is then
/// not drawn, and its width is an estimate.
pub fn available() -> bool {
    ui(11.0, Weight::Regular).set.primary.is_some()
}

impl Font {
    /// A number that is the same for the same family and weight.
    pub fn family_id(&self) -> u32 {
        self.set_id
    }

    pub fn ascent(&self) -> f32 {
        self.set.primary.as_ref().map_or(0.8, |f| f.ascent) * self.size
    }

    /// Positive.
    pub fn descent(&self) -> f32 {
        self.set.primary.as_ref().map_or(0.2, |f| f.descent) * self.size
    }

    pub fn cap_height(&self) -> f32 {
        self.set.primary.as_ref().map_or(0.7, |f| f.cap_height) * self.size
    }

    fn face_for(&self, c: char) -> Option<(&Arc<Face>, GlyphId)> {
        let primary = self.set.primary.as_ref()?;
        if let Some(id) = primary.glyph(c) {
            return Some((primary, id));
        }
        self.set
            .fallbacks
            .iter()
            .find_map(|face| face.glyph(c).map(|id| (face, id)))
    }

    /// Lays out `text`. `tracking` is extra space after every glyph, in points.
    pub fn layout(&self, text: &str, tracking: f32) -> Line {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut glyphs = Vec::with_capacity(text.len());
        let mut x = 0.0f32;
        let mut previous: Option<(u32, GlyphId)> = None;
        for c in text.chars() {
            let Some((face, glyph)) = self.face_for(c) else {
                // No font has it: leave the room of an average glyph.
                x += self.size * 0.5 + tracking;
                previous = None;
                continue;
            };
            let digit = self.set.tabular_digits && c.is_ascii_digit();
            if let Some((face_id, last)) = previous {
                if face_id == face.id && !digit {
                    x += face.font.kern_unscaled(last, glyph) / face.units_per_em * self.size;
                }
            }
            let own = face.advance(glyph);
            let (advance, shift) = if digit && face.digit_advance > 0.0 {
                (face.digit_advance, (face.digit_advance - own) * 0.5)
            } else {
                (own, 0.0)
            };
            glyphs.push(PlacedGlyph {
                face: face.id,
                glyph: glyph.0,
                x: x + shift * self.size,
            });
            x += advance * self.size + tracking;
            previous = if digit { None } else { Some((face.id, glyph)) };
        }
        Line { glyphs, width: x, id }
    }

    /// The typographic width of `text` in points.
    pub fn measure(&self, text: &str, tracking: f32) -> f32 {
        self.layout(text, tracking).width
    }
}

// MARK: Rasterization

/// The coverage of one glyph at one pixel size and sub-pixel position.
struct Bitmap {
    /// The left edge and the top edge from the pen position, pixels. `top` is negative above the baseline.
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    coverage: Vec<u8>,
}

/// Positions between two pixels a glyph can have.
const SUBPIXEL_STEPS: u32 = 4;

type BitmapKey = (u32, u16, u32, u32);

fn bitmap_cache() -> &'static Mutex<HashMap<BitmapKey, Arc<Bitmap>>> {
    static CACHE: OnceLock<Mutex<HashMap<BitmapKey, Arc<Bitmap>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn bitmap(face: &Face, glyph: u16, em_px: f32, step: u32) -> Arc<Bitmap> {
    let key = (face.id, glyph, em_px.to_bits(), step);
    let mut cache = bitmap_cache().lock().unwrap();
    if cache.len() > 8192 {
        cache.clear();
    }
    cache
        .entry(key)
        .or_insert_with(|| {
            let positioned = Glyph {
                id: GlyphId(glyph),
                scale: PxScale::from(em_px * face.px_scale_per_em),
                position: ab_glyph::point(step as f32 / SUBPIXEL_STEPS as f32, 0.0),
            };
            let Some(outlined) = face.font.outline_glyph(positioned) else {
                return Arc::new(Bitmap {
                    left: 0,
                    top: 0,
                    width: 0,
                    height: 0,
                    coverage: Vec::new(),
                });
            };
            let bounds = outlined.px_bounds();
            let (width, height) = (bounds.width() as usize, bounds.height() as usize);
            let mut coverage = vec![0u8; width * height];
            outlined.draw(|x, y, c| {
                let i = y as usize * width + x as usize;
                if i < coverage.len() {
                    coverage[i] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            });
            Arc::new(Bitmap {
                left: bounds.min.x as i32,
                top: bounds.min.y as i32,
                width,
                height,
                coverage,
            })
        })
        .clone()
}

/// Draws a laid-out line into premultiplied RGBA pixels.
///
/// `x` and `baseline` are in points, `scale` is pixels per point. `color` is
/// RGBA, not premultiplied. The baseline goes to the nearest pixel row. A glyph
/// goes to the nearest quarter of a pixel in x.
#[allow(clippy::too_many_arguments)]
pub fn draw_line(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    line: &Line,
    size: f32,
    x: f32,
    baseline: f32,
    scale: f32,
    color: [f32; 4],
) {
    let faces = book().faces.lock().unwrap();
    let em_px = size * scale;
    let base_y = (baseline * scale).round() as i32;
    let alpha = color[3].clamp(0.0, 1.0);
    let rgb = [
        color[0].clamp(0.0, 1.0) * alpha,
        color[1].clamp(0.0, 1.0) * alpha,
        color[2].clamp(0.0, 1.0) * alpha,
    ];
    for placed in &line.glyphs {
        let Some(face) = faces.get(placed.face as usize) else {
            continue;
        };
        let pen = (x + placed.x) * scale;
        let whole = pen.floor();
        let mut step = ((pen - whole) * SUBPIXEL_STEPS as f32).round() as u32;
        let mut pen_x = whole as i32;
        if step >= SUBPIXEL_STEPS {
            step = 0;
            pen_x += 1;
        }
        let glyph = bitmap(face, placed.glyph, em_px, step);
        for row in 0..glyph.height {
            let py = base_y + glyph.top + row as i32;
            if py < 0 || py >= height as i32 {
                continue;
            }
            for column in 0..glyph.width {
                let px = pen_x + glyph.left + column as i32;
                if px < 0 || px >= width as i32 {
                    continue;
                }
                let coverage = glyph.coverage[row * glyph.width + column];
                if coverage == 0 {
                    continue;
                }
                let k = coverage as f32 / 255.0;
                let i = (py as usize * width + px as usize) * 4;
                let keep = 1.0 - alpha * k;
                for c in 0..3 {
                    pixels[i + c] = (rgb[c] * k * 255.0 + pixels[i + c] as f32 * keep + 0.5) as u8;
                }
                pixels[i + 3] = (alpha * k * 255.0 + pixels[i + 3] as f32 * keep + 0.5) as u8;
            }
        }
    }
}
