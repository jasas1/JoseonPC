//! Where the cards of the main window stand: pure geometry, in points.
//!
//! Three presets. Each is a tree of splits with shares and minimums; the
//! session timeline is a strip of a fixed height under them. The numbers are
//! the ones of the macOS window, where they were chosen from snapshots.
//!
//! Port source: `rebuildLayout` in `MainWindowController.swift`,
//! `PersistentSplitView` in `PanelCardView.swift`, `TimelineStrip.swift`.

use crate::settings::{AppSettings, LayoutPreset};
use joseon_render::text::Rect;

/// A card of the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Card {
    Spectrum,
    Spectrogram,
    Vectorscope,
    Meters,
    /// "Spectrum" layout only: stereo placement beside the spectrogram.
    Placement,
    Timeline,
}

impl Card {
    pub const ALL: [Card; 6] = [
        Self::Spectrum,
        Self::Spectrogram,
        Self::Vectorscope,
        Self::Meters,
        Self::Placement,
        Self::Timeline,
    ];
}

pub const HEADER_HEIGHT: f32 = 52.0;
pub const BANNER_HEIGHT: f32 = 38.0;
pub const FLAG_STRIP_HEIGHT: f32 = 34.0;
/// The gap between two cards.
pub const GAP: f32 = 8.0;
pub const MIN_WINDOW: (f32, f32) = (900.0, 560.0);
/// The timeline strip: the user drags it between these heights.
pub const STRIP_HEIGHT_RANGE: (f32, f32) = (100.0, 300.0);
/// "Spectrum" layout: the placement card shows only when it gets at least this height.
pub const PLACEMENT_MINIMUM_CARD_HEIGHT: f32 = 300.0;
const PLACEMENT_ROW_SHARE: f32 = 0.52;
/// A card: the title row, and the frame around the panel.
pub const CARD_TITLE_HEIGHT: f32 = 28.0;
pub const CARD_BORDER: f32 = 1.0;
pub const CARD_RADIUS: f32 = 10.0;

/// The height of the area over the cards: header, banners, flag strip.
pub fn top_height(banners: usize) -> f32 {
    HEADER_HEIGHT + banners as f32 * BANNER_HEIGHT + FLAG_STRIP_HEIGHT
}

/// The lengths of the panes of a split of `length` points: by share, and no pane under its minimum
/// while the others have room to give. The start and the length of each pane.
pub fn split(length: f32, shares: &[f64], minimums: &[f32]) -> Vec<(f32, f32)> {
    let count = shares.len();
    let usable = length - GAP * (count.saturating_sub(1)) as f32;
    let total: f64 = shares.iter().sum();
    if count == 0 || !(usable > 0.0) || !(total > 0.0) {
        return vec![(0.0, 0.0); count];
    }
    let mut lengths: Vec<f32> = shares.iter().map(|s| usable * (*s / total) as f32).collect();
    let minimum = |i: usize| minimums.get(i).copied().unwrap_or(0.0);
    if (0..count).map(minimum).sum::<f32>() <= usable {
        for _ in 0..count {
            let mut missing = 0.0;
            for i in 0..count {
                if lengths[i] < minimum(i) {
                    missing += minimum(i) - lengths[i];
                    lengths[i] = minimum(i);
                }
            }
            if !(missing > 0.5) {
                break;
            }
            let room: Vec<f32> = (0..count).map(|i| (lengths[i] - minimum(i)).max(0.0)).collect();
            let total_room: f32 = room.iter().sum();
            if !(total_room > 0.0) {
                break;
            }
            for i in 0..count {
                lengths[i] -= missing * room[i] / total_room;
            }
        }
    }
    // Dividers stand on whole points, as on macOS.
    let mut panes = Vec::with_capacity(count);
    let (mut start, mut position) = (0.0f32, 0.0f32);
    for (i, pane) in lengths.iter().enumerate() {
        position += pane;
        let end = if i + 1 == count { length } else { position.round() };
        panes.push((start, (end - start).max(0.0)));
        start = end + GAP;
        position += GAP;
    }
    panes
}

/// Height of the timeline strip for a total height, or `None` when it has to hide.
pub fn strip_height(total: f32, content_minimum: f32, wanted: f32) -> Option<f32> {
    let room = total - GAP - content_minimum;
    if !(room >= STRIP_HEIGHT_RANGE.0) {
        return None;
    }
    Some(
        wanted
            .max(STRIP_HEIGHT_RANGE.0)
            .min(STRIP_HEIGHT_RANGE.1)
            .min(room)
            .floor(),
    )
}

/// A divider the pointer can drag.
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    /// The key of its split in the settings.
    pub key: String,
    /// Index of the pane before the divider.
    pub index: usize,
    /// True when the panes stand side by side (the divider moves in x).
    pub side_by_side: bool,
    /// The gap itself, for the pointer.
    pub rect: Rect,
    /// The whole split, to turn a pointer position into shares.
    pub bounds: Rect,
    pub minimums: Vec<f32>,
    /// The lengths of the panes now.
    pub lengths: Vec<f32>,
}

/// The layout of the window at one size.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowLayout {
    /// What the layout is made of: preset, placement card, strip. A change means other cards.
    pub key: String,
    pub header: Rect,
    /// Under the header: one row per banner.
    pub banners: Vec<Rect>,
    pub flag_strip: Rect,
    pub body: Rect,
    /// The cards on screen, each with its frame.
    pub cards: Vec<(Card, Rect)>,
    pub dividers: Vec<Divider>,
    /// The divider over the timeline strip, when the strip shows.
    pub strip_divider: Option<Rect>,
}

impl WindowLayout {
    pub fn card(&self, card: Card) -> Option<Rect> {
        self.cards.iter().find(|(c, _)| *c == card).map(|(_, r)| *r)
    }

    /// The panel inside a card: under the title row, inside the frame.
    pub fn panel_rect(card: Rect) -> Rect {
        Rect::new(
            card.x + CARD_BORDER,
            card.y + CARD_TITLE_HEIGHT,
            (card.w - CARD_BORDER * 2.0).max(1.0),
            (card.h - CARD_TITLE_HEIGHT - CARD_BORDER).max(1.0),
        )
    }

    /// The card under a point.
    pub fn card_at(&self, x: f32, y: f32) -> Option<(Card, Rect)> {
        self.cards.iter().copied().find(|(_, r)| r.contains_point(x, y))
    }
}

/// One split of the tree: its key, its direction, and for every pane a share, a minimum and what is in it.
struct Split<'a> {
    key: &'a str,
    side_by_side: bool,
    panes: Vec<(f64, f32, Pane<'a>)>,
}

enum Pane<'a> {
    Card(Card),
    Split(Split<'a>),
}

fn place(split_tree: &Split<'_>, area: Rect, settings: &AppSettings, out: &mut WindowLayout) {
    let defaults: Vec<f64> = split_tree.panes.iter().map(|p| p.0).collect();
    let key = format!("split.v2.{}", split_tree.key);
    let shares = settings
        .splits
        .get(&key)
        .filter(|stored| stored.len() == defaults.len())
        .cloned()
        .unwrap_or(defaults);
    let minimums: Vec<f32> = split_tree.panes.iter().map(|p| p.1).collect();
    let length = if split_tree.side_by_side { area.w } else { area.h };
    let panes = split(length, &shares, &minimums);
    let rect_of = |start: f32, len: f32| {
        if split_tree.side_by_side {
            Rect::new(area.x + start, area.y, len, area.h)
        } else {
            Rect::new(area.x, area.y + start, area.w, len)
        }
    };
    for (i, ((start, len), (_, _, pane))) in panes.iter().zip(&split_tree.panes).enumerate() {
        let rect = rect_of(*start, *len);
        match pane {
            Pane::Card(card) => out.cards.push((*card, rect)),
            Pane::Split(inner) => place(inner, rect, settings, out),
        }
        if i + 1 < panes.len() {
            out.dividers.push(Divider {
                key: key.clone(),
                index: i,
                side_by_side: split_tree.side_by_side,
                rect: rect_of(start + len, GAP),
                bounds: area,
                minimums: minimums.clone(),
                lengths: panes.iter().map(|p| p.1).collect(),
            });
        }
    }
}

/// True when the "Spectrum" layout has room for the placement card.
pub fn placement_fits(body_height: f32, show_timeline: bool) -> bool {
    let mut height = body_height;
    // With the timeline strip on, the card has to fit above the strip at its smallest. So the strip never comes
    // and goes as the window grows: a higher window first shows the strip, then the strip and the card.
    if show_timeline {
        height -= STRIP_HEIGHT_RANGE.0 + GAP;
    }
    (height - GAP) * PLACEMENT_ROW_SHARE >= PLACEMENT_MINIMUM_CARD_HEIGHT
}

/// The layout of a window of `width` x `height` points with `banners` banner rows.
pub fn window_layout(width: f32, height: f32, banners: usize, settings: &AppSettings) -> WindowLayout {
    use Card::*;
    let mut out = WindowLayout::default();
    let top = top_height(banners);
    out.header = Rect::new(0.0, 0.0, width, HEADER_HEIGHT);
    out.banners = (0..banners)
        .map(|i| Rect::new(0.0, HEADER_HEIGHT + i as f32 * BANNER_HEIGHT, width, BANNER_HEIGHT))
        .collect();
    out.flag_strip = Rect::new(0.0, top - FLAG_STRIP_HEIGHT, width, FLAG_STRIP_HEIGHT);
    let body = Rect::new(
        10.0,
        top + 1.0,
        (width - 20.0).max(1.0),
        (height - top - 1.0 - 8.0).max(1.0),
    );
    out.body = body;

    let preset = settings.layout_preset;
    let with_placement = preset == LayoutPreset::Spectrum
        && settings.spectrum_shows_placement
        && placement_fits(body.h, settings.show_timeline);
    let card = |share: f64, minimum: f32, card: Card| (share, minimum, Pane::Card(card));
    // Looked at in snapshots at 900, 1080 and 1280 pt: the stereo card needs about a third of the row, or the
    // scope is a thumbnail above its table.
    let (tree, panels_minimum) = match preset {
        LayoutPreset::Essential => (
            Split {
                key: "essential.main",
                side_by_side: false,
                panes: vec![
                    card(0.53, 229.0, Spectrum),
                    (
                        0.47,
                        228.0,
                        Pane::Split(Split {
                            key: "essential.bottom",
                            side_by_side: true,
                            panes: vec![
                                card(0.37, 240.0, Spectrogram),
                                card(0.31, 260.0, Vectorscope),
                                card(0.32, 264.0, Meters),
                            ],
                        }),
                    ),
                ],
            },
            229.0 + GAP + 228.0,
        ),
        LayoutPreset::Spectrum if with_placement => (
            Split {
                key: "spectrum.withPlacement",
                side_by_side: false,
                panes: vec![
                    card(1.0 - PLACEMENT_ROW_SHARE as f64, 200.0, Spectrum),
                    (
                        PLACEMENT_ROW_SHARE as f64,
                        PLACEMENT_MINIMUM_CARD_HEIGHT,
                        Pane::Split(Split {
                            key: "spectrum.bottom",
                            side_by_side: true,
                            panes: vec![card(0.68, 320.0, Spectrogram), card(0.32, 280.0, Placement)],
                        }),
                    ),
                ],
            },
            200.0 + GAP + PLACEMENT_MINIMUM_CARD_HEIGHT,
        ),
        // 200 pt: the spectrum keeps its legend row. 140 pt: the spectrogram keeps a readable frequency axis.
        LayoutPreset::Spectrum => (
            Split {
                key: "spectrum.main",
                side_by_side: false,
                panes: vec![card(0.55, 200.0, Spectrum), card(0.45, 140.0, Spectrogram)],
            },
            200.0 + GAP + 140.0,
        ),
        // The field alone. 200 pt keeps the near row and the frequency labels readable.
        LayoutPreset::Waterfall => (
            Split {
                key: "waterfall.main",
                side_by_side: false,
                panes: vec![card(1.0, 200.0, Spectrogram)],
            },
            200.0,
        ),
        LayoutPreset::Metering => (
            Split {
                key: "metering.main",
                side_by_side: true,
                panes: vec![
                    card(0.58, 420.0, Meters),
                    (
                        0.42,
                        340.0,
                        Pane::Split(Split {
                            key: "metering.side",
                            side_by_side: false,
                            panes: vec![card(0.58, 230.0, Vectorscope), card(0.42, 170.0, Spectrum)],
                        }),
                    ),
                ],
            },
            230.0 + GAP + 170.0,
        ),
    };
    // The panels above come first. When the window cannot give them their minimum and the strip its wish, the
    // strip shrinks down to 100 pt; with less room than that it hides, and the panels get the whole area.
    // Under the waterfall the strip starts squashed (loudness and peak only) and remembers its own height.
    let wanted = if preset == LayoutPreset::Waterfall {
        settings.waterfall_strip_height
    } else {
        settings.timeline_strip_height
    } as f32;
    let strip = settings
        .show_timeline
        .then(|| strip_height(body.h, panels_minimum, wanted))
        .flatten();
    let mut panels = body;
    if let Some(strip) = strip {
        panels.h = body.h - GAP - strip;
        out.cards
            .push((Timeline, Rect::new(body.x, body.max_y() - strip, body.w, strip)));
        out.strip_divider = Some(Rect::new(body.x, panels.max_y(), body.w, GAP));
    }
    place(&tree, panels, settings, &mut out);
    out.key = format!(
        "{}{}{}",
        preset.key(),
        if with_placement { "+placement" } else { "" },
        if strip.is_some() { "+timeline" } else { "" }
    );
    out
}

/// The shares of a split after the pointer moved one of its dividers to `position` (x or y in the window).
/// No pane goes under its minimum.
pub fn drag_divider(divider: &Divider, position: f32) -> Vec<f64> {
    let origin = if divider.side_by_side {
        divider.bounds.x
    } else {
        divider.bounds.y
    };
    let i = divider.index;
    let mut lengths = divider.lengths.clone();
    let before: f32 = lengths[..i].iter().sum::<f32>() + GAP * i as f32;
    let pair = lengths[i] + lengths[i + 1];
    let minimum = |k: usize| divider.minimums.get(k).copied().unwrap_or(0.0).min(pair / 2.0);
    let first = (position - GAP / 2.0 - origin - before)
        .max(minimum(i))
        .min(pair - minimum(i + 1));
    lengths[i] = first;
    lengths[i + 1] = pair - first;
    let total: f32 = lengths.iter().sum();
    lengths.iter().map(|l| (*l / total) as f64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(width: f32, height: f32, change: impl FnOnce(&mut AppSettings)) -> WindowLayout {
        let mut settings = AppSettings::default();
        change(&mut settings);
        window_layout(width, height, 0, &settings)
    }

    fn assert_tiles(l: &WindowLayout) {
        for (card, r) in &l.cards {
            assert!(r.w >= 150.0 && r.h >= 100.0, "{card:?} is too small: {r:?}");
            assert!(
                r.x >= l.body.x - 0.01 && r.max_x() <= l.body.max_x() + 0.01,
                "{card:?} leaves the body: {r:?}"
            );
            assert!(
                r.y >= l.body.y - 0.01 && r.max_y() <= l.body.max_y() + 0.01,
                "{card:?} leaves the body: {r:?}"
            );
            assert_eq!(
                (r.x.fract(), r.y.fract()),
                (0.0, 0.0),
                "{card:?} stands on whole points"
            );
            for (other, o) in &l.cards {
                assert!(card == other || !r.intersects(o), "{card:?} and {other:?} overlap");
            }
        }
    }

    #[test]
    fn splits_follow_the_shares_and_keep_the_minimums() {
        assert_eq!(split(1008.0, &[0.5, 0.5], &[]), [(0.0, 500.0), (508.0, 500.0)]);
        // 864 pt for three cards: 320 / 268 / 276 by share is under two minimums.
        let panes = split(864.0, &[0.37, 0.31, 0.32], &[240.0, 260.0, 264.0]);
        let lengths: Vec<f32> = panes.iter().map(|p| p.1).collect();
        assert!(
            lengths[0] >= 240.0 && lengths[1] >= 260.0 && lengths[2] >= 264.0,
            "{lengths:?}"
        );
        assert_eq!(panes[2].0 + panes[2].1, 864.0);
        assert_eq!(panes[1].0, panes[0].1 + GAP);
        // Not enough room for the minimums at all: the shares alone.
        let tight = split(300.0, &[0.5, 0.5], &[400.0, 400.0]);
        assert_eq!(tight, [(0.0, 146.0), (154.0, 146.0)]);
        assert_eq!(split(5.0, &[0.5, 0.5], &[]), [(0.0, 0.0), (0.0, 0.0)]);
    }

    #[test]
    fn the_strip_gives_way_to_the_panels() {
        assert_eq!(strip_height(700.0, 465.0, 140.0), Some(140.0));
        assert_eq!(strip_height(600.0, 465.0, 140.0), Some(127.0));
        assert_eq!(strip_height(573.0, 465.0, 140.0), Some(100.0));
        assert_eq!(strip_height(572.0, 465.0, 140.0), None);
        assert_eq!(strip_height(2000.0, 465.0, 900.0), Some(300.0));
        assert_eq!(strip_height(2000.0, 465.0, 10.0), Some(100.0));
    }

    #[test]
    fn essential_has_the_spectrum_over_three_cards() {
        let l = layout(1280.0, 800.0, |_| {});
        assert_eq!(l.key, "essential+timeline");
        assert_eq!((l.header.h, l.flag_strip.y, l.body.y), (52.0, 52.0, 87.0));
        assert_eq!(l.body, Rect::new(10.0, 87.0, 1260.0, 705.0));
        assert_tiles(&l);
        let (spectrum, strip) = (l.card(Card::Spectrum).unwrap(), l.card(Card::Timeline).unwrap());
        assert_eq!((strip.h, strip.max_y()), (140.0, 792.0));
        assert_eq!((spectrum.x, spectrum.y, spectrum.w), (10.0, 87.0, 1260.0));
        let row: Vec<Rect> = [Card::Spectrogram, Card::Vectorscope, Card::Meters]
            .map(|c| l.card(c).unwrap())
            .to_vec();
        assert!(row
            .iter()
            .all(|r| r.y == spectrum.max_y() + GAP && r.max_y() == strip.y - GAP));
        assert!(row[0].x < row[1].x && row[1].x < row[2].x);
        assert_eq!(row[2].max_x(), 1270.0);
        assert_eq!((l.dividers.len(), l.strip_divider.is_some()), (3, true));
        assert!(l.card(Card::Placement).is_none());
        // The panel of a card: under the title row.
        let panel = WindowLayout::panel_rect(spectrum);
        assert_eq!(
            (panel.x, panel.y, panel.w, panel.max_y()),
            (11.0, 115.0, 1258.0, spectrum.max_y() - 1.0)
        );
        assert_eq!(l.card_at(20.0, 100.0).map(|c| c.0), Some(Card::Spectrum));
        assert_eq!(l.card_at(20.0, 60.0), None);

        // The smallest window: the strip hides, every card keeps its minimum.
        let small = layout(900.0, 560.0, |_| {});
        assert_eq!(small.key, "essential");
        assert_tiles(&small);
        assert!(small.card(Card::Spectrum).unwrap().h >= 229.0);
        assert!(small.card(Card::Vectorscope).unwrap().w >= 260.0);
        // A banner takes its height from the body.
        let settings = AppSettings {
            show_timeline: false,
            ..AppSettings::default()
        };
        let with_banner = window_layout(1280.0, 800.0, 2, &settings);
        assert_eq!((with_banner.banners.len(), with_banner.body.y), (2, 163.0));
        assert_eq!(with_banner.key, "essential");
    }

    #[test]
    fn the_other_presets_tile_too() {
        let spectrum = |w: f32, h: f32, timeline: bool, placement: bool| {
            layout(w, h, |s| {
                s.layout_preset = LayoutPreset::Spectrum;
                s.show_timeline = timeline;
                s.spectrum_shows_placement = placement;
            })
        };
        let l = spectrum(1280.0, 800.0, false, true);
        assert_eq!(l.key, "spectrum+placement");
        assert_tiles(&l);
        assert!(l.card(Card::Placement).unwrap().h >= PLACEMENT_MINIMUM_CARD_HEIGHT);
        // With the strip the card needs a higher window; without the setting it never shows.
        assert_eq!(spectrum(1280.0, 760.0, true, true).key, "spectrum+timeline");
        assert_eq!(spectrum(1280.0, 1000.0, true, true).key, "spectrum+placement+timeline");
        assert_eq!(spectrum(1280.0, 1000.0, true, false).key, "spectrum+timeline");
        assert_eq!(spectrum(900.0, 560.0, false, true).key, "spectrum");
        assert_tiles(&spectrum(900.0, 560.0, true, true));

        let m = layout(1280.0, 800.0, |s| s.layout_preset = LayoutPreset::Metering);
        assert_eq!(m.key, "metering+timeline");
        assert_tiles(&m);
        let (meters, scope, spectrum_card) = (
            m.card(Card::Meters).unwrap(),
            m.card(Card::Vectorscope).unwrap(),
            m.card(Card::Spectrum).unwrap(),
        );
        assert!(meters.x < scope.x && scope.y < spectrum_card.y && meters.w > scope.w);
        assert!(m.card(Card::Spectrogram).is_none());
        for (w, h) in [(900.0, 560.0), (1080.0, 640.0), (1920.0, 1080.0), (2560.0, 1400.0)] {
            for preset in LayoutPreset::ALL {
                assert_tiles(&layout(w, h, |s| s.layout_preset = preset));
            }
        }
    }

    #[test]
    fn the_waterfall_layout_is_the_spectrogram_card_over_a_squashed_strip() {
        let l = layout(1280.0, 800.0, |s| s.layout_preset = LayoutPreset::Waterfall);
        assert_eq!(l.key, "waterfall+timeline");
        assert_tiles(&l);
        let field = l.card(Card::Spectrogram).unwrap();
        let strip = l.card(Card::Timeline).unwrap();
        assert_eq!(strip.h, 100.0, "squashed by default");
        assert!(field.h > 400.0 && field.w == l.body.w);
        assert!(l.card(Card::Spectrum).is_none() && l.card(Card::Meters).is_none());
        // Its own height: the strip of the other layouts keeps its 140 pt.
        let own = layout(1280.0, 800.0, |s| {
            s.layout_preset = LayoutPreset::Waterfall;
            s.waterfall_strip_height = 180.0;
        });
        assert_eq!(own.card(Card::Timeline).unwrap().h, 180.0);
        let essential = layout(1280.0, 800.0, |s| s.waterfall_strip_height = 180.0);
        assert_eq!(essential.card(Card::Timeline).unwrap().h, 140.0);
        // Without the strip the field has the whole body.
        let whole = layout(1280.0, 800.0, |s| {
            s.layout_preset = LayoutPreset::Waterfall;
            s.show_timeline = false;
        });
        assert_eq!(whole.key, "waterfall");
        assert_eq!(whole.card(Card::Spectrogram).unwrap().h, whole.body.h);
    }

    #[test]
    fn a_dragged_divider_changes_the_shares_and_stops_at_the_minimums() {
        let mut settings = AppSettings::default();
        let l = window_layout(1280.0, 800.0, 0, &settings);
        let main = l
            .dividers
            .iter()
            .find(|d| d.key == "split.v2.essential.main")
            .unwrap()
            .clone();
        assert!(!main.side_by_side);
        let spectrum = l.card(Card::Spectrum).unwrap();
        assert_eq!(main.rect, Rect::new(10.0, spectrum.max_y(), 1260.0, GAP));
        // 20 pt down.
        let shares = drag_divider(&main, main.rect.mid_y() + 20.0);
        assert!((shares.iter().sum::<f64>() - 1.0).abs() < 1e-6);
        settings.splits.insert(main.key.clone(), shares);
        let moved = window_layout(1280.0, 800.0, 0, &settings);
        assert_eq!(moved.card(Card::Spectrum).unwrap().h, spectrum.h + 20.0);
        // Far down: the row under it keeps its 228 pt.
        settings.splits.insert(main.key.clone(), drag_divider(&main, 5_000.0));
        let low = window_layout(1280.0, 800.0, 0, &settings);
        assert_eq!(low.card(Card::Meters).unwrap().h, 228.0);
        assert_tiles(&moved);
        // Far up: the spectrum stops at its 229 pt.
        settings.splits.insert(main.key.clone(), drag_divider(&main, 0.0));
        let top = window_layout(1280.0, 800.0, 0, &settings);
        assert_eq!(top.card(Card::Spectrum).unwrap().h, 229.0);
        // A middle divider of the row moves only its two neighbors.
        let row = l
            .dividers
            .iter()
            .find(|d| d.key == "split.v2.essential.bottom" && d.index == 1)
            .unwrap();
        let before = l.card(Card::Spectrogram).unwrap();
        settings.splits.clear();
        settings
            .splits
            .insert(row.key.clone(), drag_divider(row, row.rect.mid_x() - 30.0));
        let after = window_layout(1280.0, 800.0, 0, &settings);
        assert_eq!(after.card(Card::Spectrogram).unwrap(), before);
        assert_eq!(
            after.card(Card::Vectorscope).unwrap().w,
            l.card(Card::Vectorscope).unwrap().w - 30.0
        );
    }
}
