//! Tests against a saved copy of a real `HPImageArchive.aspx` reply
//! (`tests/fixtures/archive.json`, fetched on 2026-11-09). No network.

use bingwall::bing;
use bingwall::store::{self, Entry, State};

const FIXTURE: &str = include_str!("fixtures/archive.json");

#[test]
fn fixture_parses_into_eight_images_newest_first() {
    let images = bing::parse_archive(FIXTURE).unwrap();
    assert_eq!(images.len(), 8);
    let first = &images[0];
    assert_eq!(first.date, "20261109");
    assert_eq!(first.hsh, "24e7edf576b87093c4290a3e8eb157c3");
    assert_eq!(first.title, "Where art finds its rhythm");
    assert_eq!(first.urlbase, "/th?id=OHR.TrianaSpain_EN-US0872031458");
    assert_eq!(images[1].date, "20261108");
    assert_eq!(images[1].title, "A celebration that shines");
    // Dates go down by one day for each entry in this sample.
    let dates: Vec<_> = images.iter().map(|i| i.date.as_str()).collect();
    let mut sorted = dates.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(dates, sorted);
}

#[test]
fn fixture_images_have_usable_urls_and_file_names() {
    for img in bing::parse_archive(FIXTURE).unwrap() {
        assert!(img.urlbase.starts_with("/th?id=OHR."), "{}", img.urlbase);
        assert!(bing::uhd_path(&img.urlbase).ends_with("_UHD.jpg"));
        assert!(bing::fallback_path(&img.urlbase).ends_with("_1920x1080.jpg"));
        let name = store::file_name(&img.date, &img.hsh);
        assert_eq!(name.len(), "YYYYMMDD_hhhhhhhh.jpg".len(), "{name}");
        assert!(name.starts_with(&img.date));
        assert!(!name.contains(['/', '\\', ':', '?']));
    }
    let first = &bing::parse_archive(FIXTURE).unwrap()[0];
    assert_eq!(store::file_name(&first.date, &first.hsh), "20261109_24e7edf5.jpg");
    assert_eq!(
        bing::uhd_path(&first.urlbase),
        "/th?id=OHR.TrianaSpain_EN-US0872031458_UHD.jpg"
    );
}

#[test]
fn fixture_through_the_store() {
    let mut st = State::default();
    for img in bing::parse_archive(FIXTURE).unwrap() {
        st.add(Entry::new(&img.date, &img.hsh, &img.title));
    }
    assert_eq!(st.entries().len(), 8);
    assert_eq!(st.newest().unwrap().date, "20261109");
    assert!(st.is_new_day("20261109"));

    // Walk all the way round with Next: every image once, then back at the start.
    st.current = st.newest().map(Entry::id);
    let start = st.current.clone();
    let mut seen = vec![start.clone().unwrap()];
    for _ in 0..8 {
        let next = st.step(1).map(Entry::id).unwrap();
        st.current = Some(next.clone());
        seen.push(next);
    }
    assert_eq!(seen.first(), seen.last(), "eight steps wrap back to the start");
    seen.pop();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 8);

    // The text form survives a round trip.
    assert_eq!(State::parse(&st.serialize()), st);
}
