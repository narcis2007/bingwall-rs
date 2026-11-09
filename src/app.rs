//! The actions of the program: sync with Bing, step through the cache, apply a
//! wallpaper. Used by both the command line and the tray.
//!
//! All changes to the state file go through `update`, which takes a lock, reads
//! the file fresh, changes it and writes it back. Downloads happen outside the
//! lock, so a slow network never blocks a menu click.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use crate::bing::{self, Image};
use crate::store::{self, Entry, State};
use crate::win::{self, http};

/// Fetch a second API page while the cache is smaller than this. Bing lists
/// about 15 images in all, so a full cache stops asking.
const WANT_MIN: usize = 15;
const MAX_JSON: usize = 512 * 1024;
const MAX_IMAGE: usize = 40 * 1024 * 1024;
/// Smaller than this is an error page, not a photo.
const MIN_IMAGE: usize = 10 * 1024;
const JSON_TIMEOUT: Duration = Duration::from_secs(30);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(60);
const STALE_PART: Duration = Duration::from_secs(10 * 60);

pub struct Ctx {
    pub dir: PathBuf,
    pub market: String,
}

impl Ctx {
    /// The data directory is `BINGWALL_DIR` if set, else `%LOCALAPPDATA%\bingwall`.
    pub fn new(market: String) -> Result<Ctx, String> {
        let dir = match std::env::var_os("BINGWALL_DIR").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => {
                let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
                PathBuf::from(base).join("bingwall")
            }
        };
        let dir = std::path::absolute(&dir).map_err(|e| format!("bad data directory: {e}"))?;
        Ok(Ctx { dir, market })
    }

    fn images_dir(&self) -> PathBuf {
        self.dir.join(store::IMAGES_DIR)
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(store::STATE_FILE)
    }

    fn image_path(&self, e: &Entry) -> PathBuf {
        self.images_dir().join(e.file_name())
    }
}

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Read the state. Entries whose image file is gone are dropped.
fn load(ctx: &Ctx) -> State {
    let mut st = State::load(&ctx.state_path());
    st.retain_existing(|e| ctx.image_path(e).is_file());
    st
}

/// Read, change and write the state under the lock.
fn update<R>(ctx: &Ctx, f: impl FnOnce(&mut State) -> R) -> Result<R, String> {
    let _guard = lock();
    let mut st = load(ctx);
    let r = f(&mut st);
    st.save(&ctx.state_path()).map_err(|e| format!("cannot save the state: {e}"))?;
    Ok(r)
}

/// Make `id` the wallpaper and the current image.
fn apply(ctx: &Ctx, st: &mut State, id: &str) -> Result<Entry, String> {
    let e = st.get(id).cloned().ok_or("that image is not cached")?;
    win::set_wallpaper(&ctx.image_path(&e))?;
    st.current = Some(e.id());
    Ok(e)
}

pub const NOTHING_CACHED: &str = "no images cached yet; run \"bingwall today\"";

/// Move to the next (`+1`, older) or previous (`-1`, newer) cached image.
pub fn step(ctx: &Ctx, delta: isize) -> Result<Entry, String> {
    update(ctx, |st| {
        let id = st.step(delta).map(Entry::id).ok_or(NOTHING_CACHED)?;
        apply(ctx, st, &id)
    })?
}

/// Make the newest cached image the wallpaper. Does not use the network.
pub fn show_newest(ctx: &Ctx) -> Result<Entry, String> {
    update(ctx, |st| {
        let id = st.newest().map(Entry::id).ok_or(NOTHING_CACHED)?;
        let e = apply(ctx, st, &id)?;
        st.mark_applied(&e.date);
        Ok(e)
    })?
}

/// Sync, then show the newest image. If Bing cannot be reached but there are
/// cached images, the newest cached one is used and the problem is returned too.
pub fn today(ctx: &Ctx) -> Result<(Entry, Option<String>), String> {
    let problem = sync(ctx).err();
    match show_newest(ctx) {
        Ok(e) => Ok((e, problem)),
        Err(e) => Err(problem.unwrap_or(e)),
    }
}

/// One line per cached image: index (1 is the newest), date, title. `*` marks the current one.
pub fn list(ctx: &Ctx) -> Vec<String> {
    let st = load(ctx);
    let current = st.current.clone();
    st.entries()
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mark = if current.as_deref() == Some(e.id().as_str()) { '*' } else { ' ' };
            format!("{:>2}{} {}  {}", i + 1, mark, store::pretty_date(&e.date), e.title)
        })
        .collect()
}

/// Text for the tray tooltip: the title of the current image.
pub fn title(ctx: &Ctx) -> String {
    match load(ctx).current_entry() {
        Some(e) if !e.title.is_empty() => e.title.clone(),
        _ => "bingwall".to_string(),
    }
}

/// Fetch the image list, download what is missing, apply a new day's image,
/// trim the cache. Any error leaves the cache and the wallpaper as they were;
/// the caller just tries again later.
pub fn sync(ctx: &Ctx) -> Result<(), String> {
    let images_dir = ctx.images_dir();
    fs::create_dir_all(&images_dir).map_err(|e| format!("cannot create {}: {e}", images_dir.display()))?;
    remove_stale_parts(&images_dir);

    let client = http::Client::new(bing::HOST)?;
    let mut listed = fetch_index(&client, ctx, 0)?;
    if listed.is_empty() {
        return Err("Bing returned no images".to_string());
    }
    if load(ctx).entries().len() < WANT_MIN {
        // A thin cache (first run): one more page, so Next and Previous have room.
        if let Ok(older) = fetch_index(&client, ctx, bing::PAGE_SIZE) {
            listed.extend(older);
        }
    }
    listed.sort_by(|a, b| b.date.cmp(&a.date)); // stable: Bing's order within a day

    let mut first_error = None;
    let mut failures = 0;
    for (n, img) in listed.iter().enumerate() {
        let entry = Entry::new(&img.date, &img.hsh, &img.title);
        if !load(ctx).contains(&entry.id()) {
            match fetch_image(&client, img, &ctx.image_path(&entry)) {
                Ok(()) => update(ctx, |st| st.add(entry.clone()))?,
                Err(e) => {
                    failures += 1;
                    first_error.get_or_insert(e);
                    // Without the newest image there is nothing to apply, and
                    // two failures in a row mean the network is the problem.
                    if n == 0 || failures >= 2 {
                        break;
                    }
                    continue;
                }
            }
        }
        if n == 0 {
            // A failed wallpaper change is retried on the next sync.
            if let Err(e) = apply_if_new_day(ctx, &entry) {
                first_error.get_or_insert(e);
            }
        }
    }
    prune(ctx)?;
    first_error.map_or(Ok(()), Err)
}

/// Apply the newest image if it is a day that was never applied.
fn apply_if_new_day(ctx: &Ctx, newest: &Entry) -> Result<(), String> {
    update(ctx, |st| {
        if st.contains(&newest.id()) && st.is_new_day(&newest.date) {
            apply(ctx, st, &newest.id())?;
            st.mark_applied(&newest.date);
        }
        Ok(())
    })?
}

/// Keep the newest `KEEP` images (and the current one); delete the files of the rest.
fn prune(ctx: &Ctx) -> Result<(), String> {
    let dropped = update(ctx, |st| st.retain_newest(store::KEEP))?;
    store::delete_files(&ctx.images_dir(), &dropped);
    Ok(())
}

/// Delete `.part` files left by a crash. A young one may belong to a download in progress.
fn remove_stale_parts(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for f in rd.flatten() {
        let p = f.path();
        let old = f.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
        if p.extension().is_some_and(|x| x == "part") && old.is_some_and(|age| age > STALE_PART) {
            let _ = fs::remove_file(p);
        }
    }
}

fn fetch_index(client: &http::Client, ctx: &Ctx, idx: u32) -> Result<Vec<Image>, String> {
    let mut resp = client.get(&bing::index_path(idx, &ctx.market), JSON_TIMEOUT)?;
    if resp.status != 200 {
        return Err(format!("Bing answered HTTP {}", resp.status));
    }
    let mut body = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if body.len() + n > MAX_JSON {
            return Err("Bing's reply is too large".to_string());
        }
        body.extend_from_slice(&buf[..n]);
    }
    bing::parse_archive(&String::from_utf8_lossy(&body))
}

enum Got {
    Saved,
    /// The server answered, but not with 200.
    Status(u32),
}

/// Download the UHD image; if Bing has none, the 1920x1080 one. A file that is
/// already there is complete (it was renamed from a `.part` file), so it is kept.
fn fetch_image(client: &http::Client, img: &Image, dest: &Path) -> Result<(), String> {
    if dest.is_file() {
        return Ok(());
    }
    let first = match download(client, &bing::uhd_path(&img.urlbase), dest)? {
        Got::Saved => return Ok(()),
        Got::Status(s) => s,
    };
    match download(client, &bing::fallback_path(&img.urlbase), dest)? {
        Got::Saved => Ok(()),
        Got::Status(s) => Err(format!("image not available (HTTP {first}, then {s})")),
    }
}

/// Save one URL to `dest`, through a `.part` file that is renamed when complete.
fn download(client: &http::Client, path: &str, dest: &Path) -> Result<Got, String> {
    let mut resp = client.get(path, IMAGE_TIMEOUT)?;
    if resp.status != 200 {
        return Ok(Got::Status(resp.status));
    }
    let part = dest.with_extension(format!("{}.part", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut file = File::create(&part).map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        let mut buf = [0u8; 16 * 1024];
        let (mut head, mut head_len, mut total) = ([0u8; 3], 0usize, 0usize);
        loop {
            let n = resp.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let take = (head.len() - head_len).min(n);
            head[head_len..head_len + take].copy_from_slice(&buf[..take]);
            head_len += take;
            total += n;
            if total > MAX_IMAGE {
                return Err("image is too large".to_string());
            }
            file.write_all(&buf[..n]).map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        }
        if total < MIN_IMAGE || head != [0xFF, 0xD8, 0xFF] {
            return Err("Bing did not send a JPEG image".to_string());
        }
        drop(file);
        fs::rename(&part, dest).map_err(|e| format!("cannot save {}: {e}", dest.display()))
    })();
    match result {
        Ok(()) => Ok(Got::Saved),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}
