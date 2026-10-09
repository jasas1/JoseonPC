//! Port of `AutoEQParserTests.swift`, `EmbeddedCurvesTests.swift`,
//! `HeadphoneLibraryTests.swift` and the interpolation and normalization tests
//! of `HeadphoneModelTests.swift`.

use super::*;
use crate::curve::{parse_autoeq_csv, CurveInterpolator, CurveParseError};
use crate::embedded_curves::{self, HEADPHONES, STANDARD_FREQUENCIES_HZ, TARGETS};
use crate::library::HeadphoneLibrary;
use std::collections::HashSet;
use std::path::PathBuf;

fn parse(csv: &str) -> Result<HeadphoneCurve, CurveParseError> {
    parse_autoeq_csv(csv, "X", "unit test")
}

fn rows(count: usize, row: impl Fn(usize) -> String) -> Vec<String> {
    (0..count).map(row).collect()
}

// MARK: Parser

#[test]
fn autoeq_csv_picks_raw_column() {
    let csv = "frequency,raw,smoothed,error,equalization,equalized_raw,target
20.00,-6.91,-6.92,-10.18,6.00,-0.91,3.27
20.20,-6.87,-6.86,-10.16,6.00,-0.87,3.30
100.00,-1.50,-1.49,-2.00,1.00,-0.50,0.50
200.00,-0.50,-0.49,-1.00,1.00,0.50,0.50
1000.00,0.00,0.01,0.00,0.00,0.00,0.00
2000.00,1.20,1.21,-1.00,1.00,2.20,2.20
8000.00,-3.00,-3.01,2.00,-2.00,-5.00,-1.00
16000.00,-9.00,-9.01,5.00,-5.00,-14.00,-4.00
20000.00,-12.00,-12.01,6.00,-6.00,-18.00,-6.00";
    let c = parse(csv).unwrap();
    assert_eq!((c.frequencies_hz.len(), c.levels_db.len()), (9, 9));
    assert_near(c.levels_db[0], -6.91, 1e-4, "raw, not smoothed");
    assert_near(c.levels_db[5], 1.20, 1e-4, "raw");
    assert_near(c.levels_db[8], -12.00, 1e-4, "not equalized_raw (−18)");
    assert_eq!((c.name.as_str(), c.source.as_str()), ("X", "unit test"));
}

#[test]
fn two_plain_columns_no_header() {
    let csv = rows(10, |i| format!("{},{:.1}", 20 * (1 << i), i as f32 * 0.5)).join("\n");
    let c = parse(&csv).unwrap();
    assert_eq!(c.frequencies_hz.len(), 10);
    assert_eq!(c.frequencies_hz[0], 20.0);
    assert_near(c.levels_db[3], 1.5, 1e-5, "level");
}

#[test]
fn tab_separated() {
    let csv = rows(12, |i| format!("{}\t{:.1}", 20 + i * 100, -(i as f32))).join("\n");
    let c = parse(&csv).unwrap();
    assert_eq!(c.frequencies_hz.len(), 12);
    assert_near(c.levels_db[11], -11.0, 1e-5, "level");
}

#[test]
fn space_separated_with_comments_rew_style() {
    let mut lines = vec![
        "* Measurement data measured by REW V5.20".to_string(),
        "* Source: sound card".to_string(),
        "# Freq(Hz) SPL(dB) Phase(degrees)".to_string(),
        String::new(),
    ];
    lines.extend(rows(9, |i| format!("  {}   {}   0.0", 20 + i * 250, i as f32 * -0.25)));
    let c = parse(&lines.join("\r\n")).unwrap();
    assert_eq!(c.frequencies_hz.len(), 9);
    assert_near(c.frequencies_hz[0], 20.0, 1e-5, "frequency");
    assert_near(c.levels_db[8], -2.0, 1e-5, "level");
}

#[test]
fn header_without_raw_uses_next_column() {
    let mut lines = vec!["Frequency,SPL".to_string()];
    lines.extend(rows(9, |i| format!("{},{}", 100 + i * 100, i)));
    let c = parse(&lines.join("\n")).unwrap();
    assert_eq!(c.frequencies_hz.len(), 9);
    assert_near(c.levels_db[4], 4.0, 1e-5, "level");
}

#[test]
fn bad_rows_are_dropped() {
    let mut lines = vec!["frequency,raw".to_string()];
    lines.extend(rows(10, |i| format!("{},{}", 100 + i * 100, i)));
    lines.extend(
        [
            "not,numbers",
            "500",
            "-40,1.0",
            "0,2.0",
            "700,NaNish",
            ",,",
            "800,",
            "900,inf",
            "nan,3",
        ]
        .map(String::from),
    );
    let c = parse(&lines.join("\n")).unwrap();
    assert_eq!(c.frequencies_hz.len(), 10);
    assert!(c.frequencies_hz.iter().all(|&f| f > 0.0));
    assert!(c.levels_db.iter().all(|level| level.is_finite()));
}

#[test]
fn too_few_rows_and_empty_input_are_no_data() {
    assert_eq!(
        parse("frequency,raw\n20,0\n100,1\n1000,2\n"),
        Err(CurveParseError::NoData)
    );
    assert_eq!(parse(""), Err(CurveParseError::NoData));
    assert_eq!(parse("# only a comment\n"), Err(CurveParseError::NoData));
}

#[test]
fn out_of_order_rows_are_sorted() {
    let freqs = [1000, 20, 500, 8000, 100, 2000, 40, 16000, 300, 60];
    let csv: Vec<String> = freqs.iter().map(|f| format!("{f},{}", *f as f32 / 1000.0)).collect();
    let c = parse(&csv.join("\n")).unwrap();
    let mut sorted: Vec<f32> = freqs.iter().map(|&f| f as f32).collect();
    sorted.sort_by(f32::total_cmp);
    assert_eq!(c.frequencies_hz, sorted);
    assert_near(c.levels_db[0], 0.02, 1e-5, "level of the lowest point");
}

#[test]
fn duplicate_frequencies_are_averaged() {
    let mut lines = rows(9, |i| format!("{},{}", 100 + i * 100, i));
    lines.push("100,4.0".to_string()); // duplicate of the first row (value 0)
    let c = parse(&lines.join("\n")).unwrap();
    assert_eq!(c.frequencies_hz.len(), 9);
    assert_near(c.levels_db[0], 2.0, 1e-5, "mean of the duplicates");
}

#[test]
fn semicolons_and_an_hz_header_are_read() {
    let mut lines = vec!["// a note".to_string(), "Hz;dB".to_string()];
    lines.extend(rows(8, |i| format!("{};{}", 50 * (i + 1), i)));
    let c = parse(&lines.join("\n")).unwrap();
    assert_eq!((c.frequencies_hz[0], c.levels_db[7]), (50.0, 7.0));
}

// MARK: Embedded curves

fn everything() -> Vec<HeadphoneCurve> {
    HEADPHONES.iter().chain(&TARGETS).map(|curve| curve.curve()).collect()
}

#[test]
fn curves_and_targets_are_present() {
    assert!(HEADPHONES.len() >= 10 && TARGETS.len() >= 3);
    let names: HashSet<&str> = HEADPHONES.iter().map(|curve| curve.name).collect();
    for wanted in [
        "HiFiMAN Susvara Unveiled",
        "Sennheiser HD 600",
        "Sennheiser HD 650",
        "Sennheiser HD 800 S",
        "Focal Utopia",
        "Meze Empyrean (leather earpads)",
        "Apple AirPods Max",
        "Apple AirPods Pro 2",
        "Sony WH-1000XM5",
    ] {
        assert!(names.contains(wanted), "missing embedded curve {wanted}");
    }
    for wanted in ["Harman over-ear 2018", "Harman in-ear 2019"] {
        assert!(TARGETS.iter().any(|target| target.name == wanted));
    }
}

#[test]
fn every_curve_is_well_formed() {
    for c in everything() {
        assert_eq!(c.frequencies_hz.len(), c.levels_db.len(), "{}: ragged arrays", c.name);
        assert!(c.frequencies_hz.len() >= 50, "{}: too few points", c.name);
        assert!(!c.source.is_empty(), "{}: no source", c.name);
        assert!(
            c.frequencies_hz.windows(2).all(|pair| pair[1] > pair[0]),
            "{}: frequencies not strictly ascending",
            c.name
        );
        assert!(
            c.frequencies_hz[0] <= 20.0 && *c.frequencies_hz.last().unwrap() >= 20_000.0,
            "{}: range",
            c.name
        );
        // Real measurements live inside a sane dB window.
        assert!(
            c.levels_db
                .iter()
                .all(|level| level.is_finite() && *level > -80.0 && *level < 80.0),
            "{}: level",
            c.name
        );
    }
}

#[test]
fn curve_names_are_unique_and_curves_differ() {
    let names: Vec<String> = everything().into_iter().map(|curve| curve.name).collect();
    assert_eq!(
        names.iter().collect::<HashSet<_>>().len(),
        names.len(),
        "duplicate curve names"
    );
    // Guards against a generation bug that writes the same block many times.
    let signatures: HashSet<String> = HEADPHONES
        .iter()
        .map(|curve| format!("{:?}", &curve.levels_db[..20]))
        .collect();
    assert_eq!(signatures.len(), HEADPHONES.len());
}

#[test]
fn flat_target_is_flat_and_all_curves_share_the_standard_grid() {
    assert!(embedded_curves::FLAT.levels_db.iter().all(|&level| level == 0.0));
    assert_eq!(embedded_curves::FLAT.name, "Flat (zero)");
    assert_eq!(STANDARD_FREQUENCIES_HZ.len(), 200);
    for c in everything() {
        assert_eq!(c.frequencies_hz, STANDARD_FREQUENCIES_HZ, "{}: off-grid", c.name);
    }
}

// MARK: Library

/// A directory for one test, removed at the end.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("joseon-headphones-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sample_csv() -> String {
    let mut lines = vec!["frequency,raw".to_string()];
    lines.extend(rows(40, |i| {
        format!(
            "{:.2},{:.2}",
            20.0 * 1_000f64.powf(i as f64 / 39.0),
            (i as f64 / 5.0).sin() * 3.0
        )
    }));
    lines.join("\n")
}

#[test]
fn embedded_curves_are_always_available() {
    let temp = TempDir::new("embedded");
    let library = HeadphoneLibrary::new(temp.0.join("Curves"));
    assert_eq!(library.all_curves().len(), HEADPHONES.len());
    assert_eq!(library.all_targets().len(), TARGETS.len());
    assert!(library.curve_named("Sennheiser HD 650").is_some());
    assert!(library.target_named("Harman over-ear 2018").is_some());
    assert!(library.curve_named("No Such Headphone").is_none());
    assert!(
        library.user_curves().is_empty(),
        "a missing directory gives no user curves"
    );
}

#[test]
fn import_round_trip() {
    let temp = TempDir::new("import");
    let curves_dir = temp.0.join("Curves");
    let library = HeadphoneLibrary::new(&curves_dir);
    let source = temp.write("My Cans.csv", &sample_csv());
    let imported = library.import_curve(&source).unwrap();
    assert_eq!(
        (
            imported.name.as_str(),
            imported.source.as_str(),
            imported.frequencies_hz.len()
        ),
        ("My Cans", "User import", 40)
    );
    assert!(curves_dir.join("My Cans.csv").exists());

    // A new library reads it back from disk.
    let reread = HeadphoneLibrary::new(&curves_dir);
    let user = reread.user_curves();
    assert_eq!(user.len(), 1);
    assert_eq!(
        (user[0].name.as_str(), &user[0].levels_db),
        ("My Cans", &imported.levels_db)
    );
    assert_eq!(reread.all_curves().len(), HEADPHONES.len() + 1);
    assert!(reread.curve_named("My Cans").is_some());

    // A second import replaces the file. An import of the file in the library itself is fine too.
    library.import_curve(&source).unwrap();
    library.import_curve(&curves_dir.join("My Cans.csv")).unwrap();
    assert_eq!(library.user_curves().len(), 1);
}

#[test]
fn import_rejects_a_bad_file_and_leaves_nothing_behind() {
    let temp = TempDir::new("bad");
    let curves_dir = temp.0.join("Curves");
    let library = HeadphoneLibrary::new(&curves_dir);
    let bad = temp.write("Bad.csv", "hello\nthere\n");
    assert!(matches!(
        library.import_curve(&bad),
        Err(crate::library::ImportError::Parse(_))
    ));
    assert!(!curves_dir.join("Bad.csv").exists());
    assert!(matches!(
        library.import_curve(&temp.0.join("missing.csv")),
        Err(crate::library::ImportError::Io(_))
    ));
}

#[test]
fn txt_files_are_read_and_other_extensions_ignored() {
    let temp = TempDir::new("txt");
    temp.write("Curves/REW Export.txt", &sample_csv());
    temp.write("Curves/b.CSV", &sample_csv());
    temp.write("Curves/notes.md", "junk");
    temp.write("Curves/broken.csv", "junk");
    let library = HeadphoneLibrary::new(temp.0.join("Curves"));
    let names: Vec<String> = library.user_curves().into_iter().map(|curve| curve.name).collect();
    assert_eq!(
        names,
        ["REW Export", "b"],
        "sorted by file name; an upper-case extension counts"
    );
}

#[test]
fn latin1_text_is_read() {
    let temp = TempDir::new("latin1");
    let mut bytes = b"# Mesur\xe9 \xe0 Paris\n".to_vec(); // not valid UTF-8
    bytes.extend(sample_csv().bytes());
    let path = temp.0.join("old.csv");
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(
        HeadphoneLibrary::new(temp.0.join("Curves"))
            .import_curve(&path)
            .unwrap()
            .frequencies_hz
            .len(),
        40
    );
}

// MARK: Interpolation

fn knots() -> HeadphoneCurve {
    HeadphoneCurve {
        name: "knots".into(),
        source: "synthetic".into(),
        frequencies_hz: vec![100.0, 200.0, 400.0, 800.0, 1_600.0, 3_200.0, 6_400.0, 12_800.0],
        levels_db: vec![-6.0, -3.0, 0.0, 3.0, 6.0, 3.0, 0.0, -6.0],
    }
}

#[test]
fn interpolation_is_exact_at_known_points_and_linear_in_db_over_log_frequency() {
    let knots = knots();
    let interpolator = CurveInterpolator::new(knots.clone());
    for (f, db) in knots.frequencies_hz.iter().zip(&knots.levels_db) {
        assert_near(interpolator.level(*f), *db, 1e-4, &format!("at {f} Hz"));
    }
    for (got, want) in interpolator.levels(&knots.frequencies_hz).iter().zip(&knots.levels_db) {
        assert_near(*got, *want, 1e-4, "on the grid");
    }
    // The geometric mean of 100 and 200 is the half-way point on a log axis.
    assert_near(interpolator.level((100.0f32 * 200.0).sqrt()), -4.5, 1e-3, "half way");
    assert_near(interpolator.level((800.0f32 * 1_600.0).sqrt()), 4.5, 1e-3, "half way");
}

#[test]
fn interpolation_is_flat_beyond_the_ends() {
    let interpolator = CurveInterpolator::new(knots());
    for hz in [99.0, 5.0, 0.001, 0.0, -3.0, 12_801.0, 96_000.0] {
        assert_near(interpolator.level(hz), -6.0, 1e-5, &format!("at {hz} Hz"));
    }
}

#[test]
fn grid_and_scalar_paths_agree() {
    let interpolator = CurveInterpolator::new(knots());
    let grid = grid(512);
    for (batch, &f) in interpolator.levels(&grid).iter().zip(&grid) {
        assert_near(*batch, interpolator.level(f), 1e-4, &format!("at {f} Hz"));
    }
    // A grid that goes down again is still right: the cursor starts over.
    let down_up = [5_000.0, 150.0, 300.0, 9_000.0, 120.0];
    for (batch, &f) in interpolator.levels(&down_up).iter().zip(&down_up) {
        assert_near(*batch, interpolator.level(f), 1e-4, &format!("at {f} Hz"));
    }
}

#[test]
fn grid_path_handles_a_real_embedded_curve() {
    let c = embedded_curves::HD650.curve();
    let batch = CurveInterpolator::new(c.clone()).levels(&grid(1_024));
    assert_eq!(batch.len(), 1_024);
    assert!(batch.iter().all(|level| level.is_finite()));
    // Below 20 Hz and above 20 kHz the display grid is outside the measurement.
    assert_near(batch[0], c.levels_db[0], 1e-4, "below the range");
    assert_near(batch[1_023], *c.levels_db.last().unwrap(), 1e-4, "above the range");
}

#[test]
fn empty_and_one_point_curves_do_not_panic() {
    let empty = HeadphoneCurve {
        name: "e".into(),
        source: "s".into(),
        frequencies_hz: vec![],
        levels_db: vec![],
    };
    assert_eq!(empty.range_hz(), (0.0, 0.0));
    assert_eq!(empty.reference_level_db(), 0.0);
    assert_eq!(
        CurveInterpolator::new(empty.clone()).levels(&[100.0, 200.0]),
        [0.0, 0.0]
    );
    let one = HeadphoneCurve {
        frequencies_hz: vec![500.0],
        levels_db: vec![2.5],
        ..empty
    };
    assert_eq!(one.range_hz(), (500.0, 500.0));
    assert_eq!(
        one.reference_level_db(),
        2.5,
        "no point in 800–1250 Hz: the interpolated 1 kHz level"
    );
    assert_eq!(CurveInterpolator::new(one).levels(&[100.0, 9_000.0]), [2.5, 2.5]);
}

// MARK: Normalization

#[test]
fn reference_is_the_mean_of_800_to_1250() {
    let c = curve("offset", |_| 7.5);
    assert_near(c.reference_level_db(), 7.5, 1e-4, "reference");
    assert!(c.normalized_to_1khz().levels_db.iter().all(|level| level.abs() < 1e-4));
}

#[test]
fn every_embedded_curve_normalizes_to_zero_at_1khz() {
    for c in everything() {
        let n = c.normalized_to_1khz();
        // The definition: the mean over the 800–1250 Hz band is 0 dB.
        let band: Vec<f32> = n
            .frequencies_hz
            .iter()
            .zip(&n.levels_db)
            .filter(|(f, _)| **f >= 800.0 && **f <= 1250.0)
            .map(|(_, &l)| l)
            .collect();
        assert!(!band.is_empty(), "{}: no points in 800–1250 Hz", c.name);
        assert_near(band.iter().sum::<f32>() / band.len() as f32, 0.0, 0.05, &c.name);
    }
}

/// The headphones added on 2026-10-08 are whole curves, and the three modules of the U12t differ.
#[test]
fn the_added_headphones_are_whole_curves() {
    use embedded_curves::{DT900_PRO_X, TEA_PRO, U12T_M15, U12T_M20, U12T_MX};
    for curve in [DT900_PRO_X, U12T_M15, U12T_M20, U12T_MX, TEA_PRO] {
        assert!(HEADPHONES.iter().any(|c| c.name == curve.name), "{}", curve.name);
        let (low, high) = curve
            .levels_db
            .iter()
            .fold((f32::MAX, f32::MIN), |(low, high), v| (low.min(*v), high.max(*v)));
        // A measured response moves by some dB over the band, and never by a hundred.
        assert!(
            high - low > 3.0 && high - low < 60.0,
            "{}: {low} ... {high}",
            curve.name
        );
    }
    let bass = |curve: embedded_curves::EmbeddedCurve| curve.levels_db[10];
    assert!(bass(U12T_M15) != bass(U12T_M20) && bass(U12T_M20) != bass(U12T_MX));
}
