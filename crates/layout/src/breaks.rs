//! Grapheme opportunities in one affected unbreakable unit. Source keys and byte
//! offsets belong to the caller. Both inline layout and intrinsic sizing use them.
use crate::{
    font::{self, Metrics},
    text::{self, Transform, Wrap},
};

pub(crate) const ORDINARY: u8 = 1;
pub(crate) const EMERGENCY: u8 = 2;
pub(crate) const MIN_CONTENT: u8 = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Point {
    pub item: usize,
    pub byte: Option<u32>,
    pub x: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct Run<'a> {
    pub item: usize,
    pub start: u32,
    pub text: &'a str,
    pub x: f32,
    /// Before opening edges that belong to this source, after closing edges.
    pub before: Point,
    pub metrics: Metrics,
    pub transform: Transform,
    pub spacing: (f32, f32),
    pub policy: Wrap,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Opportunity {
    pub point: Point,
    pub kinds: u8,
}

fn kinds(policy: Wrap) -> u8 {
    match policy {
        Wrap::Normal => 0,
        Wrap::BreakWord => EMERGENCY,
        Wrap::Anywhere => EMERGENCY | MIN_CONTENT,
        Wrap::All => ORDINARY | EMERGENCY | MIN_CONTENT,
    }
}

/// One segmentation pass and one source walk. Prefix widths use integer font
/// units, not repeated float addition, so a long run does not accumulate drift.
pub(crate) fn opportunities(runs: &[Run<'_>], mut word_start: bool, line_x: f32) -> Vec<Opportunity> {
    let boundaries = text::grapheme_breaks(runs.iter().map(|run| run.text));
    let Some(&total) = boundaries.last() else { return Vec::new() };
    let mut boundary = 1;
    let mut global = 0;
    let mut out = Vec::new();
    for (ri, run) in runs.iter().enumerate() {
        let (mut units, mut letters, mut spaces) = (0u32, 0u32, 0u32);
        let mut tabs = 0.0;
        for (at, c) in run.text.char_indices() {
            let previous = run.metrics.px(units) + letters as f32 * run.spacing.0 + spaces as f32 * run.spacing.1 + tabs;
            if c == '\t' {
                let stop = run.metrics.px(8 * font::LETTER) + 8.0 * (run.spacing.0 + run.spacing.1);
                let pen = line_x + run.x + previous;
                if stop > 0.0 {
                    tabs += ((pen / stop).floor() + 1.0) * stop - pen;
                }
                word_start = true;
            } else {
                units = units.saturating_add(text::transformed_width(c, run.transform, &mut word_start));
                letters += (font::width_of(c) != 0) as u32;
                spaces += matches!(c, ' ' | '\u{a0}') as u32;
            }
            let end = at + c.len_utf8();
            global += c.len_utf8();
            if boundaries.get(boundary).copied() != Some(global) {
                continue;
            }
            boundary += 1;
            if global == total {
                continue;
            }
            let mut policy = kinds(run.policy);
            let point = if end == run.text.len() {
                let Some(next) = runs[ri + 1..].iter().find(|next| !next.text.is_empty()) else { continue };
                policy |= kinds(next.policy);
                next.before
            } else {
                Point {
                    item: run.item,
                    byte: Some(run.start + end as u32),
                    x: run.x + run.metrics.px(units) + letters as f32 * run.spacing.0 + spaces as f32 * run.spacing.1 + tabs,
                }
            };
            if policy != 0 {
                out.push(Opportunity { point, kinds: policy });
            }
        }
    }
    out
}

pub(crate) fn minimum(runs: &[Run<'_>], total: f32, word_start: bool) -> f32 {
    let mut start = 0.0;
    let mut widest = 0.0f32;
    for op in opportunities(runs, word_start, 0.0) {
        if op.kinds & MIN_CONTENT == 0 {
            continue;
        }
        widest = widest.max(op.point.x - start);
        start = op.point.x;
    }
    widest.max(total - start)
}
