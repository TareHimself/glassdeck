//! Laying text out in a width: how it overflows, where each line goes, and how far scrolling text has panned.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::draw::Align;

const ELLIPSIS: &str = "…";

/// What happens to text wider than its width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Overflow {
    /// Cut where it stops fitting, ending in "…"
    Ellipsis,
    /// Scaled down to fit; below `min_size` it is cut with "…" instead
    Shrink { min_size: Option<f32> },
    /// Broken at spaces onto more lines; the last line that fits ends in "…"
    Wrap { lines: Option<u32>, line_height: f32, height: Option<f32> },
    /// Pauses, pans to the end, pauses, and starts over
    Scroll { speed: f32, pause: f32 },
    /// Drawn as is, cut off at the edge
    Clip,
}

/// The width text must fit, and what happens when it doesn't.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub width: f32,
    pub overflow: Overflow,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    /// From the text's `x`, after alignment
    pub x: f32,
    /// From the text's `y`
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Laid {
    pub lines: Vec<Line>,
    pub px: f32,
    /// Nothing is drawn outside `x..x + width` when set
    pub clip: Option<f32>,
    /// How far scrolling text pans, when it's too wide to sit still
    pub pan: Option<Pan>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pan {
    pub distance: f32,
    pub speed: f32,
    pub pause: f32,
}

impl Pan {
    /// How far the text has moved left `elapsed` seconds after it appeared.
    pub fn offset(self, elapsed: f32) -> f32 {
        let travel = self.distance / self.speed.max(1.0);
        let cycle = self.pause * 2.0 + travel;
        let t = elapsed % cycle;
        ((t - self.pause) * self.speed).clamp(0.0, self.distance)
    }
}

/// Lays `text` out at `size` px; `measure` gives a string's width at a size.
pub fn lay_out(text: &str, size: f32, align: Align, fit: Option<Fit>, measure: &dyn Fn(&str, f32) -> f32) -> Laid {
    let single = |text: String, px: f32, width: Option<f32>| {
        let x = aligned(align, width, measure(&text, px));
        Laid { lines: vec![Line { text, x, y: 0.0 }], px, clip: None, pan: None }
    };
    let Some(Fit { width, overflow }) = fit else { return single(text.to_owned(), size, None) };
    let natural = measure(text, size);
    if natural <= width && !matches!(overflow, Overflow::Wrap { .. }) {
        return single(text.to_owned(), size, Some(width));
    }
    match overflow {
        Overflow::Ellipsis => single(ellipsize(text, size, width, measure), size, Some(width)),
        Overflow::Shrink { min_size } => {
            let px = size * width / natural;
            match min_size {
                Some(min) if px < min => single(ellipsize(text, min, width, measure), min, Some(width)),
                _ => single(text.to_owned(), px, Some(width)),
            }
        }
        Overflow::Clip => Laid { clip: Some(width), ..from_the_left(text, size) },
        Overflow::Scroll { speed, pause } => {
            Laid { clip: Some(width), pan: Some(Pan { distance: natural - width, speed, pause }), ..from_the_left(text, size) }
        }
        Overflow::Wrap { lines, line_height, height } => {
            let step = size * line_height;
            let room = height.map(|h| ((h / step).floor() as usize).max(1));
            let most = match (lines, room) {
                (Some(lines), Some(room)) => Some((lines as usize).min(room)),
                (Some(lines), None) => Some(lines as usize),
                (None, room) => room,
            };
            let lines = wrap(text, size, width, most, measure)
                .into_iter()
                .enumerate()
                .map(|(i, text)| Line { x: aligned(align, Some(width), measure(&text, size)), y: i as f32 * step, text })
                .collect();
            Laid { lines, px: size, clip: None, pan: None }
        }
    }
}

/// Text too wide to align starts at the left edge, whatever its alignment.
fn from_the_left(text: &str, px: f32) -> Laid {
    Laid { lines: vec![Line { text: text.to_owned(), x: 0.0, y: 0.0 }], px, clip: None, pan: None }
}

/// Where a line of `line_width` starts, from the text's `x`: within `width`, or around `x` without one.
fn aligned(align: Align, width: Option<f32>, line_width: f32) -> f32 {
    match (align, width) {
        (Align::Left, _) => 0.0,
        (Align::Center, Some(w)) => (w - line_width) / 2.0,
        (Align::Right, Some(w)) => w - line_width,
        (Align::Center, None) => -line_width / 2.0,
        (Align::Right, None) => -line_width,
    }
}

fn ellipsize(text: &str, px: f32, width: f32, measure: &dyn Fn(&str, f32) -> f32) -> String {
    if measure(text, px) <= width {
        return text.to_owned();
    }
    let ends: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
    ends.iter()
        .rev()
        .map(|&end| format!("{}{ELLIPSIS}", text[..end].trim_end()))
        .find(|cut| measure(cut, px) <= width)
        .unwrap_or_default()
}

/// Greedy line breaking at spaces, then within a word too long for a line; at most `most` lines.
fn wrap(text: &str, px: f32, width: f32, most: Option<usize>, measure: &dyn Fn(&str, f32) -> f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let joined = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
            if measure(&joined, px) <= width {
                line = joined;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            for c in word.chars() {
                let longer = format!("{line}{c}");
                if !line.is_empty() && measure(&longer, px) > width {
                    lines.push(std::mem::replace(&mut line, c.to_string()));
                } else {
                    line = longer;
                }
            }
        }
        lines.push(line);
    }
    match most {
        Some(most) if lines.len() > most => {
            let rest = lines.split_off(most.saturating_sub(1)).join(" ");
            lines.push(ellipsize(&format!("{rest}{ELLIPSIS}"), px, width, measure));
            if let Some(last) = lines.last_mut()
                && !last.ends_with(ELLIPSIS)
            {
                last.push_str(ELLIPSIS);
            }
            lines
        }
        _ => lines,
    }
}

/// When each scrolling text first appeared, so it pans from its start and keeps its place across redraws.
#[derive(Default)]
pub struct Scrolls {
    started: HashMap<String, Instant>,
    seen: HashSet<String>,
}

impl Scrolls {
    /// Seconds since `key` started scrolling, starting it now if it is new.
    pub fn elapsed(&mut self, key: &str, now: Instant) -> f32 {
        let started = *self.started.entry(key.to_owned()).or_insert(now);
        self.seen.insert(key.to_owned());
        now.saturating_duration_since(started).as_secs_f32()
    }

    /// Forgets text the last render didn't draw; reports whether anything is still panning.
    pub fn finish_render(&mut self) -> bool {
        let seen = std::mem::take(&mut self.seen);
        self.started.retain(|key, _| seen.contains(key));
        !seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character is 10 px wide at size 10.
    fn measure(text: &str, px: f32) -> f32 {
        text.chars().count() as f32 * px
    }

    fn fit(width: f32, overflow: Overflow) -> Option<Fit> {
        Some(Fit { width, overflow })
    }

    fn texts(laid: &Laid) -> Vec<&str> {
        laid.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn text_that_fits_is_left_alone_in_every_mode() {
        for overflow in [Overflow::Ellipsis, Overflow::Shrink { min_size: None }, Overflow::Clip, Overflow::Scroll { speed: 30.0, pause: 1.0 }] {
            let laid = lay_out("tide", 10.0, Align::Right, fit(100.0, overflow), &measure);
            assert_eq!((texts(&laid), laid.px, laid.lines[0].x, laid.pan), (vec!["tide"], 10.0, 60.0, None), "{overflow:?}");
        }
    }

    #[test]
    fn ellipsis_cuts_by_measured_width() {
        let laid = lay_out("Low Tide Forever", 10.0, Align::Left, fit(80.0, Overflow::Ellipsis), &measure);
        assert_eq!(texts(&laid), ["Low Tid…"]);
    }

    #[test]
    fn shrink_scales_down_until_its_minimum_then_cuts() {
        let laid = lay_out("abcdefghij", 10.0, Align::Left, fit(50.0, Overflow::Shrink { min_size: None }), &measure);
        assert_eq!((texts(&laid), laid.px), (vec!["abcdefghij"], 5.0));
        let laid = lay_out("abcdefghij", 10.0, Align::Left, fit(50.0, Overflow::Shrink { min_size: Some(8.0) }), &measure);
        assert_eq!(laid.px, 8.0);
        assert!(texts(&laid)[0].ends_with('…') && measure(texts(&laid)[0], 8.0) <= 50.0);
    }

    #[test]
    fn wrap_breaks_at_spaces_and_ends_the_last_line_that_fits_in_an_ellipsis() {
        let wrap = |lines, height| Overflow::Wrap { lines, line_height: 1.5, height };
        let laid = lay_out("the low tide comes in", 10.0, Align::Left, fit(80.0, wrap(None, None)), &measure);
        assert_eq!(texts(&laid), ["the low", "tide", "comes in"]);
        assert_eq!(laid.lines[2].y, 30.0);
        let laid = lay_out("the low tide comes in", 10.0, Align::Left, fit(80.0, wrap(Some(2), None)), &measure);
        assert_eq!(texts(&laid), ["the low", "tide co…"]);
        let laid = lay_out("the low tide comes in", 10.0, Align::Left, fit(80.0, wrap(None, Some(20.0))), &measure);
        assert_eq!(texts(&laid), ["the low…"]);
        let laid = lay_out("abcdefghijkl", 10.0, Align::Left, fit(50.0, wrap(None, None)), &measure);
        assert_eq!(texts(&laid), ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn clip_and_scroll_keep_the_whole_text_and_clip_it() {
        let laid = lay_out("abcdefghij", 10.0, Align::Center, fit(50.0, Overflow::Clip), &measure);
        assert_eq!((texts(&laid), laid.clip, laid.lines[0].x), (vec!["abcdefghij"], Some(50.0), 0.0));
        let laid = lay_out("abcdefghij", 10.0, Align::Left, fit(50.0, Overflow::Scroll { speed: 25.0, pause: 1.0 }), &measure);
        assert_eq!(laid.pan, Some(Pan { distance: 50.0, speed: 25.0, pause: 1.0 }));
    }

    #[test]
    fn a_pan_pauses_travels_pauses_and_starts_over() {
        let pan = Pan { distance: 50.0, speed: 25.0, pause: 1.0 };
        let offsets: Vec<f32> = [0.5, 1.0, 2.0, 3.0, 3.5, 4.0, 4.5].into_iter().map(|t| pan.offset(t)).collect();
        assert_eq!(offsets, [0.0, 0.0, 25.0, 50.0, 50.0, 0.0, 0.0]);
    }

    #[test]
    fn scrolls_keep_their_start_and_forget_text_no_longer_drawn() {
        let mut scrolls = Scrolls::default();
        let start = Instant::now();
        assert_eq!(scrolls.elapsed("a", start), 0.0);
        assert!(scrolls.finish_render());
        let later = start + std::time::Duration::from_secs(2);
        assert_eq!(scrolls.elapsed("a", later), 2.0);
        assert!(scrolls.finish_render());
        assert!(!scrolls.finish_render());
        assert_eq!(scrolls.elapsed("a", later), 0.0);
    }
}
