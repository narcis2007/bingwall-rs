//! The parts of the preview popup that need no Windows: which image is shown,
//! the texts, the layout, the position of the popup and DPI scaling. The window
//! and the drawing are in `win::popup`; everything here has unit tests.

use std::path::PathBuf;
use std::time::Duration;

use crate::store::{self, Entry};

/// A rectangle in pixels. `right` and `bottom` are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// One image the popup can show.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub title: String,
    /// `2026-11-09`
    pub date: String,
    pub path: PathBuf,
}

impl Item {
    pub fn new(e: &Entry, path: PathBuf) -> Item {
        Item { id: e.id(), title: e.title.clone(), date: store::pretty_date(&e.date), path }
    }
}

// ---- browsing ----

/// Step of the left arrow: the next newer image, as the tray's Previous.
pub const PREVIOUS: isize = -1;
/// Step of the right arrow: the next older image, as the tray's Next.
pub const NEXT: isize = 1;

/// The row to show after `delta` steps from row `pos` of `len` rows. It wraps
/// around like the tray's Next and Previous, because it is the same function.
pub fn browse(len: usize, pos: usize, delta: isize) -> usize {
    store::wrap_index(len, Some(pos), delta).unwrap_or(0)
}

/// `3 / 15` for row 2 (counting from 0) of 15.
pub fn position_label(pos: usize, len: usize) -> String {
    if len == 0 {
        return "0 / 0".to_string();
    }
    format!("{} / {}", pos.min(len - 1) + 1, len)
}

// ---- text ----

const ELLIPSIS: char = '\u{2026}';

/// `text` if it fits. Otherwise the longest start of `text` that, with an
/// ellipsis added, still fits. `fits` measures a string (in pixels, in the window).
pub fn ellipsize(text: &str, fits: impl Fn(&str) -> bool) -> String {
    let text = text.trim();
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| {
        let mut s: String = chars[..keep].iter().collect();
        s.truncate(s.trim_end().len());
        s.push(ELLIPSIS);
        s
    };
    // The most characters to keep: the answer is between 0 and all but one.
    let (mut lo, mut hi) = (0, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&cut(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

// ---- DPI ----

pub const BASE_DPI: u32 = 96;

/// A length in logical pixels (at 96 dpi) as pixels at `dpi`, rounded.
pub fn scale(v: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    (v as i64 * dpi as i64 + (BASE_DPI / 2) as i64).div_euclid(BASE_DPI as i64) as i32
}

// ---- layout ----

/// Size of the popup and its parts in logical pixels.
const WINDOW: (i32, i32) = (420, 300);
const IMAGE: Rect = Rect::new(10, 10, 410, 235);
const TITLE: Rect = Rect::new(10, 240, 410, 260);
const PREV: Rect = Rect::new(10, 264, 42, 292);
const NEXT_BUTTON: Rect = Rect::new(46, 264, 78, 292);
const DATE: Rect = Rect::new(90, 264, 190, 292);
const POSITION: Rect = Rect::new(190, 264, 262, 292);
const SET: Rect = Rect::new(270, 264, 410, 292);
/// Space between the tray icon and the popup.
const GAP: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Image,
    Previous,
    Next,
    Set,
}

/// Where everything is, in window pixels at one DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub window: (i32, i32),
    pub image: Rect,
    pub title: Rect,
    pub previous: Rect,
    pub next: Rect,
    pub date: Rect,
    pub position: Rect,
    pub set: Rect,
}

fn scale_rect(r: Rect, dpi: u32) -> Rect {
    Rect::new(scale(r.left, dpi), scale(r.top, dpi), scale(r.right, dpi), scale(r.bottom, dpi))
}

pub fn layout(dpi: u32) -> Layout {
    Layout {
        window: (scale(WINDOW.0, dpi), scale(WINDOW.1, dpi)),
        image: scale_rect(IMAGE, dpi),
        title: scale_rect(TITLE, dpi),
        previous: scale_rect(PREV, dpi),
        next: scale_rect(NEXT_BUTTON, dpi),
        date: scale_rect(DATE, dpi),
        position: scale_rect(POSITION, dpi),
        set: scale_rect(SET, dpi),
    }
}

impl Layout {
    /// What is under the point (window pixels), if anything that reacts.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        [(Hit::Previous, self.previous), (Hit::Next, self.next), (Hit::Set, self.set), (Hit::Image, self.image)]
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(h, _)| h)
    }
}

/// The largest rectangle with the shape of a `src_w` x `src_h` image that fits
/// in a `box_w` x `box_h` box, centred in it. Coordinates are relative to the box.
pub fn fit_rect(src_w: u32, src_h: u32, box_w: i32, box_h: i32) -> Rect {
    if src_w == 0 || src_h == 0 || box_w <= 0 || box_h <= 0 {
        return Rect::new(0, 0, 0, 0);
    }
    let (sw, sh, bw, bh) = (src_w as i64, src_h as i64, box_w as i64, box_h as i64);
    let (w, h) = if bw * sh <= bh * sw {
        (bw, ((sh * bw + sw / 2) / sw).max(1))
    } else {
        (((sw * bh + sh / 2) / sh).max(1), bh)
    };
    let (left, top) = ((bw - w) / 2, (bh - h) / 2);
    Rect::new(left as i32, top as i32, (left + w) as i32, (top + h) as i32)
}

/// Top-left corner for a popup of `size`, just above `anchor` (the tray icon),
/// centred on it, and inside `work` (the work area of the monitor). If there is
/// no room above, it goes below the anchor, as it must for a taskbar at the top.
pub fn place(anchor: Rect, work: Rect, size: (i32, i32), gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let centre = anchor.left + anchor.width() / 2;
    let x = (centre - w / 2).min(work.right - w).max(work.left);
    let above = anchor.top - gap - h;
    let y = if above >= work.top { above } else { anchor.bottom + gap };
    let y = y.min(work.bottom - h).max(work.top);
    (x, y)
}

/// `place` with the gap of the popup, scaled to `dpi`.
pub fn place_for_dpi(anchor: Rect, work: Rect, layout: &Layout, dpi: u32) -> (i32, i32) {
    place(anchor, work, layout.window, scale(GAP, dpi))
}

// ---- tray click after losing focus ----

/// A tray click moves the focus to the taskbar, so the popup closes by itself
/// just before the click reaches us. That click must not open it again.
pub const TRAY_GUARD: Duration = Duration::from_millis(300);

/// `since_focus_close` is the time since the popup last closed because it lost focus.
pub fn ignore_tray_click(since_focus_close: Option<Duration>) -> bool {
    since_focus_close.is_some_and(|d| d < TRAY_GUARD)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- browsing ----

    #[test]
    fn browse_wraps_like_the_tray() {
        assert_eq!(browse(15, 2, NEXT), 3);
        assert_eq!(browse(15, 2, PREVIOUS), 1);
        assert_eq!(browse(15, 14, NEXT), 0, "after the oldest comes the newest");
        assert_eq!(browse(15, 0, PREVIOUS), 14, "before the newest comes the oldest");
        assert_eq!(browse(1, 0, NEXT), 0);
        assert_eq!(browse(1, 0, PREVIOUS), 0);
        assert_eq!(browse(0, 0, NEXT), 0, "an empty list does not panic");
    }

    #[test]
    fn browse_agrees_with_state_step() {
        let mut st = store::State::default();
        for d in 1..=5 {
            st.add(Entry::new(&format!("202611{d:02}"), &format!("hash{d:04}"), "t"));
        }
        for start in 0..5 {
            for delta in [NEXT, PREVIOUS] {
                st.current = Some(st.entries()[start].id());
                let stepped = st.step(delta).map(Entry::id);
                assert_eq!(stepped, Some(st.entries()[browse(5, start, delta)].id()), "from {start} by {delta}");
            }
        }
    }

    #[test]
    fn position_label_counts_from_one() {
        assert_eq!(position_label(0, 15), "1 / 15");
        assert_eq!(position_label(2, 15), "3 / 15");
        assert_eq!(position_label(14, 15), "15 / 15");
        assert_eq!(position_label(0, 0), "0 / 0");
        assert_eq!(position_label(9, 3), "3 / 3", "a row past the end is held at the last one");
    }

    // ---- text ----

    /// "Pixels" in these tests are characters, 1 each.
    fn up_to(n: usize) -> impl Fn(&str) -> bool {
        move |s: &str| s.chars().count() <= n
    }

    #[test]
    fn short_titles_are_not_changed() {
        assert_eq!(ellipsize("Where art finds its rhythm", up_to(40)), "Where art finds its rhythm");
        assert_eq!(ellipsize("  padded  ", up_to(40)), "padded");
        assert_eq!(ellipsize("", up_to(5)), "");
        assert_eq!(ellipsize("exact", up_to(5)), "exact");
    }

    #[test]
    fn long_titles_are_cut_with_an_ellipsis_that_fits() {
        let long = "A very long title that goes on and on";
        let cut = ellipsize(long, up_to(12));
        assert_eq!(cut.chars().count(), 12);
        assert_eq!(cut, "A very long\u{2026}");
        assert!(cut.ends_with('\u{2026}'));
        // One character fewer than the limit is cut one character earlier.
        assert_eq!(ellipsize(long, up_to(11)), "A very lon\u{2026}");
    }

    #[test]
    fn the_cut_does_not_leave_a_space_before_the_ellipsis() {
        assert_eq!(ellipsize("abc def ghi", up_to(5)), "abc\u{2026}", "\"abc \" + ellipsis would be 5 but ends in a space");
        assert_eq!(ellipsize("abc  def", up_to(6)), "abc\u{2026}");
    }

    #[test]
    fn ellipsize_handles_wide_characters_and_tiny_boxes() {
        // Characters outside the BMP take 4 bytes in UTF-8; the cut must stay on a boundary.
        let s = "\u{1F600}\u{1F601}\u{1F602}\u{1F603}\u{1F604}";
        assert_eq!(ellipsize(s, up_to(3)), "\u{1F600}\u{1F601}\u{2026}");
        assert_eq!(ellipsize("caf\u{e9} \u{65e5}\u{672c}\u{8a9e}", up_to(6)), "caf\u{e9}\u{2026}");
        // Nothing fits: the result is just the ellipsis (it never panics).
        assert_eq!(ellipsize("abcdef", up_to(0)), "\u{2026}");
        assert_eq!(ellipsize("abcdef", up_to(1)), "\u{2026}");
        assert_eq!(ellipsize("a", |_| false), "\u{2026}");
    }

    #[test]
    fn ellipsize_measures_with_the_given_function() {
        // Narrow and wide letters: "i" is 1 unit, other letters 3.
        let width = |s: &str| s.chars().map(|c| if c == 'i' { 1 } else { 3 }).sum::<usize>();
        let fits = |s: &str| width(s) <= 10;
        // The ellipsis is 3 units wide, so 7 narrow letters and the ellipsis fill the 10.
        assert_eq!(ellipsize("iiiiiiiiiiiiiiii", fits), "iiiiiii\u{2026}");
        // Wide letters: "abc" (9) + the ellipsis (3) is too wide, so only "ab" (6) + ellipsis fits in 9.
        assert_eq!(ellipsize("abcdefg", |s| width(s) <= 9), "ab\u{2026}");
    }

    // ---- DPI ----

    #[test]
    fn scaling_rounds_to_the_nearest_pixel() {
        assert_eq!(scale(420, 96), 420);
        assert_eq!(scale(420, 120), 525);
        assert_eq!(scale(420, 144), 630);
        assert_eq!(scale(420, 192), 840);
        assert_eq!(scale(225, 120), 281, "281.25 rounds down");
        assert_eq!(scale(225, 144), 338, "337.5 rounds up");
        assert_eq!(scale(0, 144), 0);
        assert_eq!(scale(10, 0), 10, "an unknown dpi counts as 96");
        assert_eq!(scale(-10, 144), -15);
    }

    // ---- layout ----

    #[test]
    fn layout_fits_the_window_at_common_dpis() {
        for dpi in [96, 120, 144, 168, 192, 288] {
            let l = layout(dpi);
            let window = Rect::new(0, 0, l.window.0, l.window.1);
            let parts = [l.image, l.title, l.previous, l.next, l.date, l.position, l.set];
            for r in parts {
                assert!(r.width() > 0 && r.height() > 0, "dpi {dpi}: {r:?} is empty");
                assert!(
                    r.left >= window.left && r.top >= window.top && r.right <= window.right && r.bottom <= window.bottom,
                    "dpi {dpi}: {r:?} is outside {window:?}"
                );
            }
            // The image is 16:9 within a pixel of rounding.
            let (w, h) = (l.image.width(), l.image.height());
            assert!((w * 9 - h * 16).abs() <= 16, "dpi {dpi}: image {w}x{h} is not 16:9");
            // The parts of the bottom row do not overlap, and the title is above it.
            let row = [l.previous, l.next, l.date, l.position, l.set];
            for pair in row.windows(2) {
                assert!(pair[0].right <= pair[1].left, "dpi {dpi}: {:?} overlaps {:?}", pair[0], pair[1]);
            }
            assert!(l.image.bottom <= l.title.top && l.title.bottom <= l.previous.top);
        }
        assert_eq!(layout(96).window, (420, 300));
        assert_eq!(layout(144).window, (630, 450));
    }

    #[test]
    fn hit_test_finds_the_buttons_and_the_image() {
        let l = layout(96);
        assert_eq!(l.hit(20, 270), Some(Hit::Previous));
        assert_eq!(l.hit(60, 270), Some(Hit::Next));
        assert_eq!(l.hit(300, 270), Some(Hit::Set));
        assert_eq!(l.hit(200, 100), Some(Hit::Image));
        assert_eq!(l.hit(200, 250), None, "the title does nothing");
        assert_eq!(l.hit(120, 270), None, "the date does nothing");
        assert_eq!(l.hit(-5, -5), None);
        assert_eq!(l.hit(l.window.0, l.window.1), None);
        // Edges: left and top are inside, right and bottom are outside.
        assert_eq!(l.hit(l.set.left, l.set.top), Some(Hit::Set));
        assert_eq!(l.hit(l.set.right, l.set.top), None);
        assert_eq!(l.hit(l.set.left, l.set.bottom), None);
    }

    #[test]
    fn images_are_fitted_inside_the_box() {
        // 16:9 into a 16:9 box fills it.
        assert_eq!(fit_rect(3840, 2160, 400, 225), Rect::new(0, 0, 400, 225));
        // A square image is centred with bars left and right.
        assert_eq!(fit_rect(1000, 1000, 400, 225), Rect::new(87, 0, 312, 225));
        // A very wide image has bars above and below.
        assert_eq!(fit_rect(2000, 500, 400, 225), Rect::new(0, 62, 400, 162));
        // A smaller image is scaled up to fit.
        assert_eq!(fit_rect(192, 108, 400, 225), Rect::new(0, 0, 400, 225));
        // Nothing to draw gives an empty rectangle and no panic.
        assert_eq!(fit_rect(0, 100, 400, 225).width(), 0);
        assert_eq!(fit_rect(100, 0, 400, 225).width(), 0);
        assert_eq!(fit_rect(100, 100, 0, 225).width(), 0);
        // The result is always inside the box.
        for (w, h) in [(1u32, 5000u32), (5000, 1), (3, 7), (1920, 1080)] {
            let r = fit_rect(w, h, 400, 225);
            assert!(r.left >= 0 && r.top >= 0 && r.right <= 400 && r.bottom <= 225, "{w}x{h}: {r:?}");
            assert!(r.width() >= 1 && r.height() >= 1);
        }
    }

    // ---- placement ----

    const WORK: Rect = Rect::new(0, 0, 1920, 1040); // 1080 high, taskbar of 40 at the bottom
    const SIZE: (i32, i32) = (420, 300);

    #[test]
    fn the_popup_goes_just_above_the_icon() {
        let icon = Rect::new(1500, 1044, 1524, 1076);
        let (x, y) = place(icon, WORK, SIZE, 8);
        assert_eq!(x, 1512 - 210, "centred on the icon");
        assert_eq!(y, 1044 - 8 - 300, "the gap above the icon");
        assert!(y + SIZE.1 <= WORK.bottom, "and the popup does not cover the taskbar");
    }

    #[test]
    fn the_popup_stays_inside_the_work_area() {
        let size = SIZE;
        // An icon at the right edge pushes the popup left.
        let (x, _) = place(Rect::new(1890, 1044, 1914, 1076), WORK, size, 8);
        assert_eq!(x, 1920 - 420);
        // An icon at the left edge pushes it right.
        let (x, _) = place(Rect::new(2, 1044, 26, 1076), WORK, size, 8);
        assert_eq!(x, 0);
        // On a second monitor to the right, the work area starts at 1920.
        let work2 = Rect::new(1920, 0, 3840, 1040);
        let (x, y) = place(Rect::new(3000, 1044, 3024, 1076), work2, size, 8);
        assert_eq!((x, y), (3012 - 210, 736));
        let (x, _) = place(Rect::new(1925, 1044, 1949, 1076), work2, size, 8);
        assert_eq!(x, 1920);
    }

    #[test]
    fn a_taskbar_at_the_top_puts_the_popup_below_the_icon() {
        let work = Rect::new(0, 40, 1920, 1080);
        let icon = Rect::new(1500, 4, 1524, 36);
        let (x, y) = place(icon, work, SIZE, 8);
        assert_eq!(y, 36 + 8);
        assert_eq!(x, 1512 - 210);
    }

    #[test]
    fn a_vertical_taskbar_keeps_the_popup_off_the_taskbar() {
        // Taskbar on the right: the work area ends at 1880, the icon is at x 1884..1916.
        let work = Rect::new(0, 0, 1880, 1080);
        let icon = Rect::new(1884, 900, 1916, 932);
        let (x, y) = place(icon, work, SIZE, 8);
        assert_eq!(x, 1880 - 420);
        assert_eq!(y, 900 - 8 - 300);
        assert!(x + SIZE.0 <= work.right);
    }

    #[test]
    fn placement_from_a_cursor_point_and_in_a_tiny_work_area() {
        // The cursor fallback is a 1x1 rectangle.
        let (x, y) = place(Rect::new(960, 700, 961, 701), WORK, SIZE, 8);
        assert_eq!((x, y), (960 - 210, 700 - 8 - 300));
        // A work area smaller than the popup: no panic, and the corner is the work area's.
        let tiny = Rect::new(100, 100, 300, 250);
        assert_eq!(place(Rect::new(150, 200, 170, 230), tiny, SIZE, 8), (100, 100));
        // The gap is scaled with the DPI.
        let l = layout(192);
        let (_, y) = place_for_dpi(Rect::new(1000, 1500, 1040, 1560), Rect::new(0, 0, 3840, 2200), &l, 192);
        assert_eq!(y, 1500 - 16 - 600);
    }

    // ---- tray click guard ----

    #[test]
    fn a_tray_click_right_after_a_focus_loss_is_ignored() {
        assert!(!ignore_tray_click(None), "the popup was not closed by a focus loss");
        assert!(ignore_tray_click(Some(Duration::from_millis(0))));
        assert!(ignore_tray_click(Some(Duration::from_millis(299))));
        assert!(!ignore_tray_click(Some(Duration::from_millis(300))));
        assert!(!ignore_tray_click(Some(Duration::from_secs(5))));
    }

    #[test]
    fn items_carry_what_the_popup_shows() {
        let e = Entry::new("20261109", "24e7edf576b87093c4290a3e8eb157c3", "Where art finds its rhythm");
        let item = Item::new(&e, PathBuf::from("x.jpg"));
        assert_eq!(item.id, "20261109_24e7edf5");
        assert_eq!(item.date, "2026-11-09");
        assert_eq!(item.title, "Where art finds its rhythm");
    }
}
