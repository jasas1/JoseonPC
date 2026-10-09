//! The headphone library: the embedded curves plus curves the user imported.
//!
//! Port source: `HeadphoneLibrary` in `Sources/JoseonHeadphones/HeadphoneModel.swift`.

use crate::curve::{parse_autoeq_csv, CurveParseError, HeadphoneCurve};
use crate::embedded_curves::{HEADPHONES, TARGETS};
use std::fmt;
use std::path::{Path, PathBuf};

/// Extensions the library reads out of the user curves directory.
pub const USER_CURVE_EXTENSIONS: [&str; 2] = ["csv", "txt"];

/// Why an import failed.
#[derive(Debug)]
pub enum ImportError {
    /// The file could not be read, or the copy could not be written.
    Io(std::io::Error),
    /// The file is not a curve.
    Parse(CurveParseError),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Io(error) => write!(f, "{error}"),
            ImportError::Parse(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ImportError {}

pub struct HeadphoneLibrary {
    /// Where imported user curves live.
    user_curves_directory: PathBuf,
}

impl HeadphoneLibrary {
    /// A library with its user curves in `user_curves_directory`. The app
    /// gives the place: on Windows `%APPDATA%\Joseon\Curves`.
    pub fn new(user_curves_directory: impl Into<PathBuf>) -> Self {
        Self {
            user_curves_directory: user_curves_directory.into(),
        }
    }

    pub fn user_curves_directory(&self) -> &Path {
        &self.user_curves_directory
    }

    /// Curves embedded in the crate plus curves the user imported.
    pub fn all_curves(&self) -> Vec<HeadphoneCurve> {
        let mut curves: Vec<HeadphoneCurve> = HEADPHONES.iter().map(|curve| curve.curve()).collect();
        curves.extend(self.user_curves());
        curves
    }

    /// Target curves embedded in the crate, for example "Harman over-ear 2018".
    pub fn all_targets(&self) -> Vec<HeadphoneCurve> {
        TARGETS.iter().map(|curve| curve.curve()).collect()
    }

    /// Curves parsed out of the user curves directory, sorted by file name.
    /// Files that cannot be read are skipped.
    pub fn user_curves(&self) -> Vec<HeadphoneCurve> {
        let Ok(entries) = std::fs::read_dir(&self.user_curves_directory) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        paths.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));
        paths
            .iter()
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| USER_CURVE_EXTENSIONS.contains(&extension.to_lowercase().as_str()))
            })
            .filter_map(|path| parse_file(path).ok())
            .collect()
    }

    /// Copies a CSV into the user curves directory and returns the parsed curve.
    ///
    /// The file is parsed before it is copied, so a bad file never lands in the library.
    pub fn import_curve(&self, path: &Path) -> Result<HeadphoneCurve, ImportError> {
        let curve = parse_file(path)?;
        std::fs::create_dir_all(&self.user_curves_directory).map_err(ImportError::Io)?;
        let Some(file_name) = path.file_name() else {
            return Ok(curve);
        };
        let destination = self.user_curves_directory.join(file_name);
        let same_file = match (std::fs::canonicalize(path), std::fs::canonicalize(&destination)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if !same_file {
            std::fs::copy(path, &destination).map_err(ImportError::Io)?;
        }
        Ok(curve)
    }

    /// First curve with this name, embedded or user.
    pub fn curve_named(&self, name: &str) -> Option<HeadphoneCurve> {
        self.all_curves().into_iter().find(|curve| curve.name == name)
    }

    /// First target with this name.
    pub fn target_named(&self, name: &str) -> Option<HeadphoneCurve> {
        self.all_targets().into_iter().find(|curve| curve.name == name)
    }
}

/// Reads a curve file. The curve takes the name of the file. Text that is not
/// UTF-8 is read as Latin-1, which old measurement tools write.
fn parse_file(path: &Path) -> Result<HeadphoneCurve, ImportError> {
    let bytes = std::fs::read(path).map_err(ImportError::Io)?;
    let text = String::from_utf8(bytes).unwrap_or_else(|error| error.into_bytes().iter().map(|&b| b as char).collect());
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    parse_autoeq_csv(&text, &name, "User import").map_err(ImportError::Parse)
}
