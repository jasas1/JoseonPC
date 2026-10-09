//! The strip under the header: the headphone and its stress flags on the left,
//! the loudness against the target on the right. This module lays the strip
//! out; `scene.rs` draws it and sends the clicks here.
//!
//! Port source: `StressFlagStrip`, `StressChip` and `StressChipLayout` in `HeaderView.swift`.

use crate::model::{Relation, StressState};
use joseon_core::contracts::StressSeverity;
use joseon_render::text::fonts::{self, Font, Weight};
use joseon_render::text::{OverlayContext, Rect};

pub const CAPTION: &str = "HEADPHONE STRESS";
/// Mark 13 pt + gap 4 + padding 8 + 8, and one point of rounding room.
const CHIP_CHROME: f32 = 13.0 + 4.0 + 16.0 + 1.0;
const CHIP_SPACING: f32 = 6.0;
const CHIP_HEIGHT: f32 = 22.0;
pub const DETAIL_WIDTH: f32 = 300.0;

pub fn caption_font() -> Font {
    fonts::ui(10.0, Weight::Semibold)
}

pub fn quiet_font() -> Font {
    fonts::ui(11.5, Weight::Regular)
}

pub fn chip_font() -> Font {
    fonts::ui(11.0, Weight::Medium)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    pub rect: Rect,
    pub title: String,
    pub severity: StressSeverity,
    /// The flags behind the chip: one, or the ones that did not fit ("+2").
    pub flags: Vec<usize>,
}

/// The plate with the detail of a chip, under the strip.
#[derive(Debug, Clone, PartialEq)]
pub struct Detail {
    pub rect: Rect,
    /// A title (in the color of its severity) and its wrapped text, for every flag of the chip.
    pub blocks: Vec<(String, StressSeverity, Vec<String>)>,
    pub footnote: String,
}

/// Where everything of the strip stands.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StripLayout {
    pub rect: Rect,
    /// Quiet texts: the text and its left edge.
    pub texts: Vec<(String, f32)>,
    /// The green "all is well" mark: its center.
    pub mark: Option<[f32; 2]>,
    /// "Choose headphone…".
    pub button: Option<(Rect, String)>,
    pub chips: Vec<Chip>,
    /// The loudness against the target, with its right edge.
    pub loudness: Option<(String, Relation, f32)>,
    pub detail: Option<Detail>,
    /// A click here (and not on a chip) opens the headphone menu.
    pub picker: Rect,
}

/// `text` cut into lines of at most `width` points, at spaces.
pub fn wrap(o: &OverlayContext, text: &str, font: &Font, width: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && o.measure(&candidate, font, 0.0) > width {
            lines.push(std::mem::take(&mut line));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn overflow_title(count: usize, plus: bool) -> String {
    match (plus, count) {
        (true, n) => format!("+{n}"),
        (false, 1) => "1 flag".to_string(),
        (false, n) => format!("{n} flags"),
    }
}

/// Lays the strip out in `rect`. `open` = the chip whose detail shows (its place in the row).
pub fn layout(o: &OverlayContext, rect: Rect, state: &StressState, open: Option<usize>) -> StripLayout {
    let mut out = StripLayout {
        rect,
        ..StripLayout::default()
    };
    let mid = rect.mid_y();
    let mut right = rect.max_x() - 14.0;
    if let Some(loudness) = &state.loudness {
        let font = fonts::ui(11.5, Weight::Medium);
        out.loudness = Some((loudness.text.clone(), loudness.relation, right));
        right -= o.measure(&loudness.text, &font, 0.0) + 16.0;
    }
    let mut x = rect.x + 14.0 + o.measure(CAPTION, &caption_font(), 0.4) + 10.0;
    out.picker = Rect::new(rect.x, rect.y, (right - rect.x).max(0.0), rect.h);
    let quiet = quiet_font();
    let Some(name) = &state.headphone else {
        // The short text is used when the window is narrow.
        let button = "Choose headphone\u{2026}";
        let button_w = o.measure(button, &chip_font(), 0.0) + 20.0;
        for text in ["Pick a headphone to see how it handles this music", "Pick a headphone"] {
            let w = o.measure(text, &quiet, 0.0);
            if x + w + 10.0 + button_w <= right {
                out.texts.push((text.to_string(), x));
                x += w + 10.0;
                break;
            }
        }
        out.button = Some((
            Rect::new(x, mid - CHIP_HEIGHT / 2.0, button_w, CHIP_HEIGHT),
            button.to_string(),
        ));
        return out;
    };
    if state.flags.is_empty() {
        let text = if state.waiting {
            // Nothing was measured yet: no green mark, it would reassure falsely.
            "Stress check waits for audio".to_string()
        } else {
            out.mark = Some([x + 5.0, mid]);
            x += 16.0;
            "No stress flags".to_string()
        };
        let w = o.measure(&text, &quiet, 0.0);
        out.texts.push((text, x));
        let tail = format!("\u{00B7}  {name}");
        if x + w + 8.0 + o.measure(&tail, &quiet, 0.0) <= right {
            out.texts.push((tail, x + w + 8.0));
        }
        return out;
    }
    // Whole chips only: the chips that do not fit go into one "+N" chip. Flags arrive highest severity first, so
    // the chips that show are the ones that matter most.
    let font = chip_font();
    let width_of = |title: &str| o.measure(title, &font, 0.0).ceil() + CHIP_CHROME;
    let room = right - x;
    let (mut used, mut count) = (0.0f32, 0);
    for (i, flag) in state.flags.iter().enumerate() {
        let w = width_of(&flag.title) + if i == 0 { 0.0 } else { CHIP_SPACING };
        let rest = state.flags.len() - i - 1;
        let reserve = if rest > 0 {
            CHIP_SPACING + width_of(&overflow_title(rest, true))
        } else {
            0.0
        };
        if used + w + reserve > room {
            break;
        }
        used += w;
        count += 1;
    }
    let y = mid - CHIP_HEIGHT / 2.0;
    for (i, flag) in state.flags.iter().take(count).enumerate() {
        let w = width_of(&flag.title);
        out.chips.push(Chip {
            rect: Rect::new(x, y, w, CHIP_HEIGHT),
            title: flag.title.clone(),
            severity: flag.severity,
            flags: vec![i],
        });
        x += w + CHIP_SPACING;
    }
    if count < state.flags.len() {
        let title = overflow_title(state.flags.len() - count, count > 0);
        let w = width_of(&title);
        if x + w <= right + 0.5 {
            out.chips.push(Chip {
                rect: Rect::new(x, y, w, CHIP_HEIGHT),
                title,
                severity: state.flags[count].severity,
                flags: (count..state.flags.len()).collect(),
            });
        }
    }
    // The detail of the open chip: a plate under the strip, inside the window.
    if let Some(chip) = open.and_then(|i| out.chips.get(i)) {
        let detail_font = fonts::ui(11.5, Weight::Regular);
        let blocks: Vec<(String, StressSeverity, Vec<String>)> = chip
            .flags
            .iter()
            .map(|i| &state.flags[*i])
            .map(|flag| {
                (
                    flag.title.clone(),
                    flag.severity,
                    wrap(o, &flag.detail, &detail_font, DETAIL_WIDTH - 28.0),
                )
            })
            .collect();
        let lines: usize = blocks.iter().map(|b| b.2.len()).sum();
        let height = 14.0 + blocks.len() as f32 * 22.0 + lines as f32 * 16.0 + 18.0 + 10.0;
        let left = chip.rect.x.min(rect.max_x() - DETAIL_WIDTH - 10.0).max(rect.x + 10.0);
        out.detail = Some(Detail {
            rect: Rect::new(left, rect.max_y() + 6.0, DETAIL_WIDTH, height),
            blocks,
            footnote: format!("Headphone model: {name}"),
        });
    }
    out
}

/// What a click in the strip means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The chip at this place of the row.
    Chip(usize),
    /// Open the headphone menu.
    Picker,
}

pub fn hit(layout: &StripLayout, x: f32, y: f32) -> Option<Hit> {
    if let Some(i) = layout.chips.iter().position(|chip| chip.rect.contains_point(x, y)) {
        return Some(Hit::Chip(i));
    }
    layout.picker.contains_point(x, y).then_some(Hit::Picker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LoudnessDelta;
    use crate::settings::LoudnessTarget;
    use joseon_core::contracts::StressFlag;

    fn flag(id: &str, severity: StressSeverity, title: &str) -> StressFlag {
        StressFlag {
            id: id.to_string(),
            severity,
            title: title.to_string(),
            detail: "The music asks a lot of the headphone in this range, more than most recordings do.".to_string(),
            frequency_range_hz: None,
            plot_label: String::new(),
        }
    }

    fn strip(width: f32) -> (OverlayContext, Rect) {
        (
            OverlayContext::new(width, 800.0, 2.0),
            Rect::new(0.0, 52.0, width, 34.0),
        )
    }

    #[test]
    fn with_no_headphone_the_strip_asks_for_one() {
        if !fonts::available() {
            return;
        }
        let (o, rect) = strip(1280.0);
        let l = layout(&o, rect, &StressState::default(), None);
        assert_eq!(l.texts[0].0, "Pick a headphone to see how it handles this music");
        let (button, title) = l.button.clone().unwrap();
        assert_eq!(title, "Choose headphone\u{2026}");
        assert!(button.x > l.texts[0].1 && button.max_x() < 1280.0);
        assert_eq!(hit(&l, button.mid_x(), button.mid_y()), Some(Hit::Picker));
        assert_eq!(hit(&l, 20.0, 20.0), None, "over the strip");
        // A narrow strip: the short text.
        let (o, rect) = strip(430.0);
        assert_eq!(
            layout(&o, rect, &StressState::default(), None).texts[0].0,
            "Pick a headphone"
        );
    }

    #[test]
    fn a_quiet_strip_names_the_headphone() {
        if !fonts::available() {
            return;
        }
        let (o, rect) = strip(1280.0);
        let mut state = StressState {
            headphone: Some("Sennheiser HD 600".to_string()),
            waiting: true,
            ..StressState::default()
        };
        let l = layout(&o, rect, &state, None);
        assert_eq!(l.texts[0].0, "Stress check waits for audio");
        assert_eq!(l.texts[1].0, "\u{00B7}  Sennheiser HD 600");
        assert!(l.mark.is_none() && l.chips.is_empty());
        state.waiting = false;
        state.loudness = LoudnessDelta::new(-11.5, LoudnessTarget::Streaming);
        let l = layout(&o, rect, &state, None);
        assert_eq!(l.texts[0].0, "No stress flags");
        assert!(l.mark.is_some());
        let (text, relation, right) = l.loudness.clone().unwrap();
        assert_eq!((relation, right), (Relation::Over, 1266.0));
        assert!(text.ends_with("LU over target"));
        // The picker ends where the loudness text begins.
        assert!(l.picker.max_x() < 1266.0 - 100.0);
    }

    #[test]
    fn chips_show_whole_or_go_into_a_count() {
        if !fonts::available() {
            return;
        }
        use StressSeverity::{High, Info, Watch};
        let state = StressState {
            headphone: Some("HD 600".to_string()),
            flags: vec![
                flag("a", High, "Strong sub-bass in the signal"),
                flag("b", Watch, "Treble peak over the target"),
                flag("c", Info, "Low crest factor"),
            ],
            ..StressState::default()
        };
        let (o, rect) = strip(1280.0);
        let l = layout(&o, rect, &state, Some(1));
        assert_eq!(
            l.chips.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            [
                "Strong sub-bass in the signal",
                "Treble peak over the target",
                "Low crest factor"
            ]
        );
        assert!(l
            .chips
            .windows(2)
            .all(|pair| pair[1].rect.x == pair[0].rect.max_x() + 6.0));
        assert_eq!(hit(&l, l.chips[1].rect.mid_x(), rect.mid_y()), Some(Hit::Chip(1)));
        // The open chip has its plate under the strip.
        let detail = l.detail.clone().unwrap();
        assert_eq!(
            (detail.rect.y, detail.rect.w, detail.rect.x),
            (92.0, 300.0, l.chips[1].rect.x)
        );
        assert_eq!(detail.blocks.len(), 1);
        assert!(
            detail.blocks[0].2.len() >= 2,
            "the detail is wrapped: {:?}",
            detail.blocks[0].2
        );
        assert_eq!(detail.footnote, "Headphone model: HD 600");
        // A narrow strip: one chip and "+2".
        let (o, rect) = strip(480.0);
        let l = layout(&o, rect, &state, Some(1));
        assert_eq!(
            l.chips.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            ["Strong sub-bass in the signal", "+2"]
        );
        assert_eq!(l.chips[1].flags, [1, 2]);
        assert_eq!(l.detail.unwrap().blocks.len(), 2, "the count chip lists its flags");
        // No room for a chip at all: the count alone.
        let (o, rect) = strip(300.0);
        let l = layout(&o, rect, &state, None);
        assert_eq!(
            l.chips.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            ["3 flags"]
        );
    }

    #[test]
    fn text_wraps_at_spaces() {
        if !fonts::available() {
            return;
        }
        let o = OverlayContext::new(400.0, 100.0, 2.0);
        let font = quiet_font();
        let lines = wrap(&o, "one two three four five six seven eight nine ten", &font, 80.0);
        assert!(lines.len() >= 3);
        assert!(lines.iter().all(|line| o.measure(line, &font, 0.0) <= 80.0));
        assert_eq!(lines.join(" "), "one two three four five six seven eight nine ten");
        assert_eq!(wrap(&o, "", &font, 80.0), Vec::<String>::new());
        assert_eq!(wrap(&o, "Supercalifragilistic", &font, 10.0), ["Supercalifragilistic"]);
    }
}
