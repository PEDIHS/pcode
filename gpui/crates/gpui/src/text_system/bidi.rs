//! Visual ordering for right-to-left scripts (Persian, Arabic, Hebrew).
//!
//! Platform text systems hand back glyphs in logical order with x growing
//! along that order, which is what wrapping, hit testing and decorations rely
//! on. The shaper has already picked the joined letter forms for right-to-left
//! runs, so the only thing left is where each glyph lands. That is decided per
//! wrapped line, after wrapping, as the Unicode Bidirectional Algorithm (UAX #9,
//! rules L1 and L2) asks, so a long Persian paragraph keeps its first words on
//! its first line.

use super::{LineLayout, WrapBoundary};
use crate::{Pixels, px};
use unicode_bidi::{BidiClass, BidiInfo, bidi_class};

/// Whether `text` holds any character from a right-to-left script. Lines
/// without one keep their logical positions and pay nothing for this module.
pub(crate) fn has_rtl(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c as u32,
            0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF
        )
    })
}

/// For every glyph of `layout`, in run then glyph order, how far it moves from
/// its logical x so that each wrapped line reads in visual order. `None` when
/// the text has nothing right-to-left in it.
pub(crate) fn visual_offsets(
    text: &str,
    layout: &LineLayout,
    wrap_boundaries: &[WrapBoundary],
) -> Option<Vec<Pixels>> {
    if !has_rtl(text) {
        return None;
    }
    let info = BidiInfo::new(text, None);

    let mut run_starts = Vec::with_capacity(layout.runs.len());
    let mut glyphs = Vec::new();
    for run in &layout.runs {
        run_starts.push(glyphs.len());
        glyphs.extend(run.glyphs.iter().map(|g| (g.index, g.position.x)));
    }
    if glyphs.is_empty() {
        return None;
    }

    let mut line_starts = vec![0];
    line_starts.extend(
        wrap_boundaries
            .iter()
            .filter_map(|b| run_starts.get(b.run_ix).map(|start| start + b.glyph_ix)),
    );
    line_starts.push(glyphs.len());

    let mut offsets = vec![px(0.); glyphs.len()];
    for line in line_starts.windows(2) {
        let (start, end) = (line[0], line[1]);
        if start >= end {
            continue;
        }
        let line_end_x = glyphs.get(end).map_or(layout.width, |&(_, x)| x);
        let char_at = |i: usize| text.get(glyphs[i].0..).and_then(|s| s.chars().next());

        // Move whole clusters: a base glyph with the marks, joiners and extra
        // glyphs that follow it. Those carry shaper offsets (DirectWrite
        // gives a ZWNJ a negative one), so their x says nothing about the
        // base's width. Only base glyphs step the pen.
        let mut clusters: Vec<(usize, usize)> = Vec::new();
        for i in start..end {
            let attached = i > start
                && (glyphs[i].0 == glyphs[i - 1].0 || char_at(i).is_some_and(is_attached));
            match clusters.last_mut() {
                Some(cluster) if attached => cluster.1 = i + 1,
                _ => clusters.push((i, i + 1)),
            }
        }
        let next_x = |cluster: usize| {
            clusters
                .get(cluster + 1)
                .map_or(line_end_x, |&(next, _)| glyphs[next].1)
        };

        let mut levels: Vec<u8> = clusters
            .iter()
            .map(|&(first, _)| level_at(&info, glyphs[first].0))
            .collect();
        // L1: whitespace at the end of a line takes the paragraph level.
        let paragraph_level = paragraph_level_at(&info, glyphs[start].0);
        for (k, &(first, _)) in clusters.iter().enumerate().rev() {
            match char_at(first) {
                Some(c) if c.is_whitespace() => levels[k] = paragraph_level,
                _ => break,
            }
        }

        let mut x = glyphs[start].1;
        for k in visual_order(&levels) {
            let (first, cluster_end) = clusters[k];
            let base_x = glyphs[first].1;
            let pen_after = next_x(k);
            offsets[first] = x - base_x;
            for i in first + 1..cluster_end {
                // Right to left, an attached glyph sits relative to the pen
                // after its base, which is the base's left edge on screen.
                let drawn = if levels[k] % 2 == 1 {
                    x + (glyphs[i].1 - pen_after)
                } else {
                    x + (glyphs[i].1 - base_x)
                };
                offsets[i] = drawn - glyphs[i].1;
            }
            x += pen_after - base_x;
        }
    }
    Some(offsets)
}

/// Marks and invisible joiners (ZWNJ, ZWJ) that belong to the glyph before them.
fn is_attached(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::NSM | BidiClass::BN)
}

fn level_at(info: &BidiInfo, byte_ix: usize) -> u8 {
    info.levels.get(byte_ix).map_or(0, |level| level.number())
}

fn paragraph_level_at(info: &BidiInfo, byte_ix: usize) -> u8 {
    info.paragraphs
        .iter()
        .find(|p| p.range.contains(&byte_ix))
        .or(info.paragraphs.last())
        .map_or(0, |p| p.level.number())
}

/// L2: from the highest level down to the lowest odd one, reverse every
/// stretch of positions at that level or higher. Returns logical indices in
/// left-to-right visual order.
fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let mut levels = levels.to_vec();
    let Some(highest) = levels.iter().copied().max() else {
        return order;
    };
    let Some(lowest_odd) = levels.iter().copied().filter(|l| l % 2 == 1).min() else {
        return order;
    };
    for level in (lowest_odd..=highest).rev() {
        let mut i = 0;
        while i < levels.len() {
            if levels[i] < level {
                i += 1;
                continue;
            }
            let run_start = i;
            while i < levels.len() && levels[i] >= level {
                i += 1;
            }
            order[run_start..i].reverse();
            levels[run_start..i].reverse();
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontId, GlyphId, ShapedGlyph, ShapedRun};

    fn layout_for(text: &str, advance: f32) -> LineLayout {
        let glyphs: Vec<ShapedGlyph> = text
            .char_indices()
            .enumerate()
            .map(|(n, (index, _))| ShapedGlyph {
                id: GlyphId(n as u32),
                position: crate::point(px(n as f32 * advance), px(0.)),
                index,
                is_emoji: false,
            })
            .collect();
        let count = glyphs.len();
        LineLayout {
            font_size: px(16.),
            width: px(count as f32 * advance),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs: glyphs.into(),
            }],
            len: text.len(),
            ..Default::default()
        }
    }

    fn painted_order(text: &str, wraps: &[WrapBoundary]) -> String {
        let layout = layout_for(text, 10.);
        let offsets = visual_offsets(text, &layout, wraps).unwrap_or_default();
        let mut placed: Vec<(f32, char)> = text
            .chars()
            .enumerate()
            .map(|(n, c)| {
                let shift = offsets.get(n).copied().unwrap_or(px(0.));
                (f32::from(px(n as f32 * 10.) + shift), c)
            })
            .collect();
        placed.sort_by(|a, b| a.0.total_cmp(&b.0));
        placed.into_iter().map(|(_, c)| c).collect()
    }

    #[test]
    fn latin_text_is_left_alone() {
        let layout = layout_for("hello world", 10.);
        assert!(visual_offsets("hello world", &layout, &[]).is_none());
    }

    #[test]
    fn persian_word_reads_right_to_left() {
        assert_eq!(painted_order("سلام", &[]), "مالس");
    }

    #[test]
    fn latin_inside_persian_keeps_its_own_direction() {
        // Paragraph is RTL: the Persian words swap sides, "abc" stays readable.
        assert_eq!(painted_order("سلام abc دنیا", &[]), "ایند abc مالس");
    }

    #[test]
    fn persian_inside_latin_paragraph() {
        assert_eq!(painted_order("go سلام now", &[]), "go مالس now");
    }

    #[test]
    fn zwnj_offset_does_not_move_the_letter_before_it() {
        // DirectWrite shapes "می‌ت" with a zero-width ZWNJ that carries an
        // advance offset, so its x is past the next letter's.
        let text = "می\u{200c}ت";
        let advances = [7.9, 11.3, 0.0, 4.5];
        let xs = [0.0, 7.9, 7.9 + 11.3 + 3.7, 19.2];
        let glyphs: Vec<ShapedGlyph> = text
            .char_indices()
            .zip(xs)
            .enumerate()
            .map(|(n, ((index, _), x))| ShapedGlyph {
                id: GlyphId(n as u32),
                position: crate::point(px(x), px(0.)),
                index,
                is_emoji: false,
            })
            .collect();
        let layout = LineLayout {
            font_size: px(16.),
            width: px(advances.iter().sum::<f32>()),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs,
            }],
            len: text.len(),
            ..Default::default()
        };
        let offsets = visual_offsets(text, &layout, &[]).unwrap();
        let drawn = |n: usize| xs[n] + f32::from(offsets[n]);
        // Right to left: م at the right edge, ی touching it, ت touching ی.
        assert!((drawn(0) - 15.8).abs() < 0.01, "م at {}", drawn(0));
        assert!((drawn(1) - 4.5).abs() < 0.01, "ی at {}", drawn(1));
        assert!((drawn(3) - 0.0).abs() < 0.01, "ت at {}", drawn(3));
    }

    #[test]
    fn each_wrapped_line_is_reordered_on_its_own() {
        // "ab cd" wrapped after the space: first line holds the first word.
        let text = "اب جد";
        let wraps = [WrapBoundary { run_ix: 0, glyph_ix: 3 }];
        let layout = layout_for(text, 10.);
        let offsets = visual_offsets(text, &layout, &wraps).unwrap();
        // First line glyphs 0..3 ("اب "), trailing space goes to the far end
        // in an RTL paragraph, so "ب" is leftmost after the space... check the
        // two letters swapped within the line and stayed inside it.
        let x = |n: usize| f32::from(px(n as f32 * 10.) + offsets[n]);
        assert!(x(0) > x(1), "first letter sits right of the second");
        assert!(x(0) < 30. && x(1) < 30., "first line glyphs stay on the first line");
        assert!(x(3) >= 30. && x(4) >= 30.);
        assert!(x(3) > x(4));
    }
}
