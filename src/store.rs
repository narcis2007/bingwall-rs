//! The on-disk state: a small text index of the cached images, the current
//! image and the date of the last automatic change. No I/O except in `load`,
//! `save` and `delete_files`.
//!
//! File format (`state.txt`, UTF-8, one record per line):
//!
//! ```text
//! bingwall-state 1
//! current 20261109_24e7edf5
//! applied 20261109
//! image<TAB>20261109<TAB>24e7edf576b87093c4290a3e8eb157c3<TAB>Where art finds its rhythm
//! ```
//!
//! The image file name is not stored; it is always `<date>_<first 8 of hsh>.jpg`.

use std::fs;
use std::io;
use std::path::Path;

/// How many images to keep on disk.
pub const KEEP: usize = 30;
pub const STATE_FILE: &str = "state.txt";
pub const IMAGES_DIR: &str = "images";

const HEADER: &str = "bingwall-state 1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// YYYYMMDD
    pub date: String,
    pub hsh: String,
    pub title: String,
}

impl Entry {
    /// Tabs and line breaks in the title would break the file format.
    pub fn new(date: &str, hsh: &str, title: &str) -> Entry {
        let title: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        Entry { date: date.to_string(), hsh: hsh.to_string(), title: title.trim().to_string() }
    }

    /// Identity of the image, also its file name without extension.
    pub fn id(&self) -> String {
        file_stem(&self.date, &self.hsh)
    }

    pub fn file_name(&self) -> String {
        file_name(&self.date, &self.hsh)
    }
}

/// `20261109` and `24e7edf5...` give `20261109_24e7edf5`.
pub fn file_stem(date: &str, hsh: &str) -> String {
    let short: String = hsh.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    format!("{date}_{short}")
}

pub fn file_name(date: &str, hsh: &str) -> String {
    format!("{}.jpg", file_stem(date, hsh))
}

/// `20261109` gives `2026-11-09`.
pub fn pretty_date(date: &str) -> String {
    match (date.get(0..4), date.get(4..6), date.get(6..8)) {
        (Some(y), Some(m), Some(d)) if date.len() == 8 => format!("{y}-{m}-{d}"),
        _ => date.to_string(),
    }
}

fn valid_record(date: &str, hsh: &str) -> bool {
    date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
        && !hsh.is_empty()
        && hsh.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct State {
    /// Newest first (by date, then by hsh).
    entries: Vec<Entry>,
    /// Id of the image that is the wallpaper now, as far as bingwall knows.
    pub current: Option<String>,
    /// Date (YYYYMMDD) of the newest image that was applied by "Today" or by a
    /// sync. A newer image than this is a new day's image.
    pub applied: Option<String>,
}

impl State {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.iter().any(|e| e.id() == id)
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id() == id)
    }

    pub fn current_entry(&self) -> Option<&Entry> {
        self.current.as_deref().and_then(|id| self.get(id))
    }

    pub fn newest(&self) -> Option<&Entry> {
        self.entries.first()
    }

    /// Add an image, or replace the entry that has the same id.
    pub fn add(&mut self, e: Entry) {
        let id = e.id();
        self.entries.retain(|x| x.id() != id);
        self.entries.push(e);
        self.entries.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.hsh.cmp(&b.hsh)));
    }

    /// Walk the list from the current image. `+1` is the next row (an older
    /// image), `-1` the previous row (a newer one). It wraps around at both ends.
    /// Without a current image, `+` gives the first row and `-` the last.
    pub fn step(&self, delta: isize) -> Option<&Entry> {
        let len = self.entries.len() as isize;
        if len == 0 {
            return None;
        }
        let pos = self.current.as_deref().and_then(|id| self.entries.iter().position(|e| e.id() == id));
        let target = match pos {
            Some(p) => (p as isize + delta).rem_euclid(len),
            None if delta >= 0 => 0,
            None => len - 1,
        };
        self.entries.get(target as usize)
    }

    /// True when `date` is newer than anything applied automatically before.
    pub fn is_new_day(&self, date: &str) -> bool {
        self.applied.as_deref().is_none_or(|a| date > a)
    }

    /// Remember that the image of `date` was applied. The value never goes back.
    pub fn mark_applied(&mut self, date: &str) {
        if self.is_new_day(date) {
            self.applied = Some(date.to_string());
        }
    }

    /// Drop entries that fail `exists` (for example, a file the user deleted).
    pub fn retain_existing(&mut self, exists: impl Fn(&Entry) -> bool) {
        self.entries.retain(|e| exists(e));
        if self.current.as_deref().is_some_and(|id| !self.contains(id)) {
            self.current = None;
        }
    }

    /// Keep the `keep` newest entries, and the current one in any case.
    /// Returns the entries that were dropped.
    pub fn retain_newest(&mut self, keep: usize) -> Vec<Entry> {
        let current = self.current.clone();
        let mut dropped = Vec::new();
        let mut rank = 0;
        let all = std::mem::take(&mut self.entries);
        for e in all {
            rank += 1;
            if rank <= keep || current.as_deref() == Some(e.id().as_str()) {
                self.entries.push(e);
            } else {
                dropped.push(e);
            }
        }
        dropped
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(HEADER);
        out.push('\n');
        if let Some(c) = &self.current {
            out.push_str(&format!("current {c}\n"));
        }
        if let Some(a) = &self.applied {
            out.push_str(&format!("applied {a}\n"));
        }
        for e in &self.entries {
            out.push_str(&format!("image\t{}\t{}\t{}\n", e.date, e.hsh, e.title));
        }
        out
    }

    /// Read the text form. Unknown or damaged lines are skipped; a file with a
    /// different header gives an empty state.
    pub fn parse(text: &str) -> State {
        let mut lines = text.lines();
        let mut st = State::default();
        if lines.next().map(str::trim_end) != Some(HEADER) {
            return st;
        }
        for line in lines {
            if let Some(rest) = line.strip_prefix("current ") {
                st.current = Some(rest.trim().to_string()).filter(|s| !s.is_empty());
            } else if let Some(rest) = line.strip_prefix("applied ") {
                st.applied = Some(rest.trim().to_string()).filter(|s| !s.is_empty());
            } else if let Some(rest) = line.strip_prefix("image\t") {
                let mut f = rest.splitn(3, '\t');
                if let (Some(date), Some(hsh), Some(title)) = (f.next(), f.next(), f.next()) {
                    if valid_record(date, hsh) {
                        st.add(Entry::new(date, hsh, title));
                    }
                }
            }
        }
        st
    }

    /// A missing or unreadable file gives an empty state.
    pub fn load(path: &Path) -> State {
        match fs::read(path) {
            Ok(bytes) => State::parse(&String::from_utf8_lossy(&bytes)),
            Err(_) => State::default(),
        }
    }

    /// Write to a temporary file, then rename it over the real one.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, self.serialize())?;
        fs::rename(&tmp, path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }
}

/// Delete the image files of `entries`. A file that is already gone is fine.
pub fn delete_files(images_dir: &Path, entries: &[Entry]) {
    for e in entries {
        let _ = fs::remove_file(images_dir.join(e.file_name()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(date: &str, hsh: &str) -> Entry {
        Entry::new(date, hsh, &format!("Title {date}"))
    }

    /// Entries for days 1..=n of November 2026, newest first after `add`.
    fn state_with(n: u32) -> State {
        let mut st = State::default();
        for d in 1..=n {
            st.add(entry(&format!("202611{d:02}"), &format!("hash{d:04}xyz")));
        }
        st
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_names() {
        assert_eq!(file_stem("20261109", "24e7edf576b87093c4290a3e8eb157c3"), "20261109_24e7edf5");
        assert_eq!(file_name("20261109", "24e7edf576b87093c4290a3e8eb157c3"), "20261109_24e7edf5.jpg");
        assert_eq!(file_name("20261109", "ab"), "20261109_ab.jpg");
        assert_eq!(file_stem("20261109", "../..\\x"), "20261109_x");
        assert_eq!(pretty_date("20261109"), "2026-11-09");
        assert_eq!(pretty_date("junk"), "junk");
    }

    #[test]
    fn titles_lose_control_characters() {
        let e = Entry::new("20261109", "ab", "  two\tlines\r\nhere ");
        assert_eq!(e.title, "two lines  here");
    }

    #[test]
    fn entries_are_sorted_and_deduplicated() {
        let mut st = State::default();
        st.add(entry("20261105", "aaaa1111"));
        st.add(entry("20261107", "bbbb2222"));
        st.add(entry("20261106", "cccc3333"));
        st.add(Entry::new("20261107", "bbbb2222", "Renamed"));
        let dates: Vec<_> = st.entries().iter().map(|e| e.date.as_str()).collect();
        assert_eq!(dates, ["20261107", "20261106", "20261105"]);
        assert_eq!(st.entries()[0].title, "Renamed");
        // Two images on the same day sort by hsh.
        st.add(entry("20261107", "0000ffff"));
        assert_eq!(st.entries()[0].hsh, "0000ffff");
        assert_eq!(st.entries()[1].hsh, "bbbb2222");
    }

    #[test]
    fn text_round_trip() {
        let mut st = state_with(4);
        st.current = Some("20261103_hash0003".into());
        st.applied = Some("20261104".into());
        st.add(Entry::new("20261110", "ffff0000", "Caf\u{e9} \u{65e5}\u{672c}"));
        let again = State::parse(&st.serialize());
        assert_eq!(again, st);
    }

    #[test]
    fn disk_round_trip_and_missing_file() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("sub").join(STATE_FILE);
        assert_eq!(State::load(&path), State::default());
        let mut st = state_with(3);
        st.current = Some("20261102_hash0002".into());
        st.save(&path).unwrap();
        assert_eq!(State::load(&path), st);
        // No temporary file is left behind.
        let names: Vec<_> = fs::read_dir(dir.join("sub")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1);
        // Saving again replaces the old file.
        st.applied = Some("20261103".into());
        st.save(&path).unwrap();
        assert_eq!(State::load(&path).applied.as_deref(), Some("20261103"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_files_are_survivable() {
        assert_eq!(State::parse(""), State::default());
        assert_eq!(State::parse("something else\nimage\t20261109\tab\tx\n"), State::default());
        let text = "bingwall-state 1\nunknown line\nimage\t20261109\n\
                    image\tbad\tab\tx\nimage\t20261108\t../x\tx\nimage\t20261107\tab12\tGood\nimage\t20261106\tcd34\n";
        let st = State::parse(text);
        assert_eq!(st.entries().len(), 1);
        assert_eq!(st.entries()[0].title, "Good");
        // An image line with an empty title is kept; the title falls back to nothing.
        let st = State::parse("bingwall-state 1\nimage\t20261106\tcd34\t\n");
        assert_eq!(st.entries().len(), 1);
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let mut st = state_with(3); // order: 03, 02, 01
        st.current = Some("20261103_hash0003".into());
        assert_eq!(st.step(1).unwrap().date, "20261102");
        assert_eq!(st.step(-1).unwrap().date, "20261101", "previous of the first row wraps to the last");
        st.current = Some("20261101_hash0001".into());
        assert_eq!(st.step(1).unwrap().date, "20261103", "next of the last row wraps to the first");
        assert_eq!(st.step(-1).unwrap().date, "20261102");
        // A full circle comes back to the start.
        let mut at = st.current.clone();
        for _ in 0..3 {
            st.current = at.clone();
            at = st.step(1).map(Entry::id);
        }
        assert_eq!(at.as_deref(), Some("20261101_hash0001"));
    }

    #[test]
    fn step_edge_cases() {
        let mut st = State::default();
        assert!(st.step(1).is_none());
        let mut one = state_with(1);
        one.current = Some("20261101_hash0001".into());
        assert_eq!(one.step(1).unwrap().date, "20261101");
        assert_eq!(one.step(-1).unwrap().date, "20261101");
        st = state_with(3);
        assert_eq!(st.step(1).unwrap().date, "20261103", "no current: next is the first row");
        assert_eq!(st.step(-1).unwrap().date, "20261101", "no current: previous is the last row");
        st.current = Some("gone".into());
        assert_eq!(st.step(1).unwrap().date, "20261103", "unknown current behaves like none");
    }

    #[test]
    fn new_day_rule() {
        let mut st = State::default();
        assert!(st.is_new_day("20261109"));
        st.mark_applied("20261109");
        assert!(!st.is_new_day("20261109"), "same day is not new");
        assert!(!st.is_new_day("20261108"), "an older day is not new");
        assert!(st.is_new_day("20261110"));
        st.mark_applied("20261101");
        assert_eq!(st.applied.as_deref(), Some("20261109"), "the value never goes back");
    }

    #[test]
    fn retention_keeps_newest_and_current() {
        let mut st = state_with(31); // days 01..31
        st.current = Some("20261105_hash0005".into());
        let dropped = st.retain_newest(KEEP);
        // 30 newest are 02..31; day 01 is dropped; current (05) is already inside.
        assert_eq!(st.entries().len(), 30);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].date, "20261101");
        assert_eq!(st.entries().last().unwrap().date, "20261102");
    }

    #[test]
    fn retention_never_drops_the_current_image() {
        let mut st = state_with(31);
        st.current = Some("20261101_hash0001".into()); // the oldest
        let dropped = st.retain_newest(KEEP);
        assert!(dropped.is_empty());
        assert_eq!(st.entries().len(), 31);
        assert!(st.contains("20261101_hash0001"));
        // With a smaller keep, the current one stays and the rest go.
        let dropped = st.retain_newest(2);
        assert_eq!(st.entries().len(), 3);
        assert_eq!(dropped.len(), 28);
        assert!(st.contains("20261101_hash0001"));
        assert_eq!(st.entries()[0].date, "20261131");
        assert_eq!(st.entries()[1].date, "20261130");
    }

    #[test]
    fn retention_deletes_the_files_of_dropped_entries() {
        let dir = temp_dir("retention");
        let mut st = state_with(5);
        st.current = Some("20261101_hash0001".into());
        for e in st.entries() {
            fs::write(dir.join(e.file_name()), b"x").unwrap();
        }
        let dropped = st.retain_newest(2);
        delete_files(&dir, &dropped);
        let mut left: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        let mut want: Vec<_> = st.entries().iter().map(Entry::file_name).collect();
        want.sort();
        assert_eq!(left, want);
        assert_eq!(left.len(), 3);
        // Deleting files that are already gone is not an error.
        delete_files(&dir, &dropped);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retain_existing_drops_missing_files_and_stale_current() {
        let mut st = state_with(3);
        st.current = Some("20261102_hash0002".into());
        st.retain_existing(|e| e.date != "20261102");
        assert_eq!(st.entries().len(), 2);
        assert_eq!(st.current, None);
    }
}
