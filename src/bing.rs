//! The Bing "HPImageArchive" API: build request paths and read the reply.
//! This module does no network I/O.

use crate::json::{self, Value};

pub const HOST: &str = "www.bing.com";
pub const DEFAULT_MARKET: &str = "en-US";
/// Bing returns at most 8 images per call.
pub const PAGE_SIZE: u32 = 8;

/// One image as listed by the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `startdate`, always 8 ASCII digits (YYYYMMDD).
    pub date: String,
    pub hsh: String,
    pub title: String,
    /// Path without size suffix, for example `/th?id=OHR.TrianaSpain_EN-US0872031458`.
    pub urlbase: String,
}

/// A market code such as `en-US` or `de-DE`. Only letters, digits and `-`.
pub fn valid_market(m: &str) -> bool {
    (2..=12).contains(&m.len()) && m.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Request path for one page of the archive. `idx` is how many images to skip.
pub fn index_path(idx: u32, market: &str) -> String {
    format!("/HPImageArchive.aspx?format=js&idx={idx}&n={PAGE_SIZE}&mkt={market}")
}

/// Path of the full-size (UHD) image.
pub fn uhd_path(urlbase: &str) -> String {
    format!("{urlbase}_UHD.jpg")
}

/// Path of the 1920x1080 image, used when UHD is not available.
pub fn fallback_path(urlbase: &str) -> String {
    format!("{urlbase}_1920x1080.jpg")
}

/// Read the reply of `HPImageArchive.aspx?format=js`. Entries that lack a usable
/// field are skipped. A reply without an `images` array is an error.
pub fn parse_archive(body: &str) -> Result<Vec<Image>, String> {
    let root = json::parse(body)?;
    let list = root.get("images").and_then(Value::as_array).ok_or("reply has no \"images\" array")?;
    Ok(list.iter().filter_map(read_image).collect())
}

fn read_image(v: &Value) -> Option<Image> {
    let date = v.get("startdate")?.as_str()?;
    let hsh = v.get("hsh")?.as_str()?;
    let urlbase = v.get("urlbase")?.as_str()?;
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // `hsh` becomes part of a file name, `urlbase` part of a request path.
    if hsh.is_empty() || !hsh.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    if !urlbase.starts_with('/') || !urlbase.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let title = v.get("title").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
    Some(Image {
        date: date.to_string(),
        hsh: hsh.to_string(),
        title: title.map(str::to_string).unwrap_or_else(|| name_from_urlbase(urlbase)),
        urlbase: urlbase.to_string(),
    })
}

/// Fallback title: `/th?id=OHR.TrianaSpain_EN-US0872031458` gives `TrianaSpain`.
fn name_from_urlbase(urlbase: &str) -> String {
    let id = urlbase.rsplit("id=").next().unwrap_or(urlbase);
    let id = id.strip_prefix("OHR.").unwrap_or(id);
    id.split('_').next().unwrap_or(id).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(index_path(0, "en-US"), "/HPImageArchive.aspx?format=js&idx=0&n=8&mkt=en-US");
        assert_eq!(index_path(8, "de-DE"), "/HPImageArchive.aspx?format=js&idx=8&n=8&mkt=de-DE");
        assert_eq!(uhd_path("/th?id=OHR.X_EN-US1"), "/th?id=OHR.X_EN-US1_UHD.jpg");
        assert_eq!(fallback_path("/th?id=OHR.X_EN-US1"), "/th?id=OHR.X_EN-US1_1920x1080.jpg");
    }

    #[test]
    fn market_validation() {
        assert!(valid_market("en-US"));
        assert!(valid_market("zh-CN"));
        assert!(valid_market("de"));
        assert!(!valid_market(""));
        assert!(!valid_market("e"));
        assert!(!valid_market("en US"));
        assert!(!valid_market("en-US&idx=5"));
        assert!(!valid_market("en-US\r\nX: y"));
    }

    #[test]
    fn skips_bad_entries_and_falls_back_for_title() {
        let body = r#"{"images":[
            {"startdate":"20261109","hsh":"ab12","urlbase":"/th?id=OHR.TrianaSpain_EN-US0872031458","title":"  Rhythm "},
            {"startdate":"2026-11-08","hsh":"ab12","urlbase":"/th?id=OHR.A_EN-US1","title":"bad date"},
            {"startdate":"20261107","hsh":"","urlbase":"/th?id=OHR.B_EN-US1","title":"empty hsh"},
            {"startdate":"20261106","hsh":"zz/..","urlbase":"/th?id=OHR.C_EN-US1","title":"bad hsh"},
            {"startdate":"20261105","hsh":"cd34","urlbase":"th?id=OHR.D_EN-US1","title":"no slash"},
            {"startdate":"20261104","hsh":"cd34","urlbase":"/th?id=OHR.D E","title":"space in path"},
            {"startdate":"20261103","hsh":"ef56","urlbase":"/th?id=OHR.Dakota_EN-US99"},
            {"startdate":"20261102","urlbase":"/th?id=OHR.E_EN-US1"},
            42
        ]}"#;
        let images = parse_archive(body).unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].title, "Rhythm");
        assert_eq!(images[1].date, "20261103");
        assert_eq!(images[1].title, "Dakota");
    }

    #[test]
    fn reply_without_images_is_an_error() {
        assert!(parse_archive("{}").is_err());
        assert!(parse_archive("{\"images\":5}").is_err());
        assert!(parse_archive("not json").is_err());
        assert_eq!(parse_archive("{\"images\":[]}").unwrap(), vec![]);
    }
}
