# bingwall

A tiny Windows tray app that sets the Bing picture of the day as your wallpaper.

It does one job. No settings window, no telemetry, no updater, no search, no favourites, no info panels.

bingwall is an independent project. It is not affiliated with, endorsed by or sponsored by Microsoft. "Bing" is a trademark of Microsoft.

## Why it is tiny

- One Rust program, one dependency (`windows-sys`, plain Win32 declarations).
- Web requests use WinHTTP, which is part of Windows. There is no TLS library, no browser engine and no async runtime in the program.
- The tray process sleeps between hourly checks. It keeps nothing in memory but its window and the icon.

## Install

Build it yourself (a recent stable Rust and the MSVC build tools):

```
git clone https://github.com/narcis2007/bingwall-rs
cd bingwall-rs
cargo build --release
```

The program is `target\release\bingwall.exe`. Copy it anywhere you like and run it. The C runtime is linked in, so no extra redistributable is needed.

To start it at logon, run `bingwall autostart on`.

## Tray mode

Run `bingwall.exe` with no arguments. A small icon appears in the notification area. Only one tray instance runs at a time; a second launch exits at once.

- On start, and then every hour (and shortly after the PC wakes from sleep), it asks Bing for the latest images and downloads the ones it does not have.
- When a new day's image arrives, it becomes your wallpaper. If there is nothing new, your wallpaper stays as it is, so a restart does not undo a picture you chose.
- Hover the icon to see the title of the current image.
- Left-click the icon: **Next**.
- Right-click for the menu:
  - **Next**: the next older cached image. After the oldest, it wraps to the newest.
  - **Previous**: the next newer cached image. Before the newest, it wraps to the oldest.
  - **Today**: the newest image.
  - **Quit**.

If the network is down, bingwall keeps your wallpaper and tries again at the next check.

## Command line

Each command does its work, prints one short line and exits.

```
bingwall today          download the newest images and set the newest one
bingwall next           set the next older cached image
bingwall prev           set the previous (newer) cached image
bingwall list           list cached images: index, date, title (* marks the current one)
bingwall autostart on   start bingwall at logon
bingwall autostart off  stop starting bingwall at logon
bingwall --market de-DE today
```

`--market CODE` works with every command and with tray mode. The default is `en-US`. Bing offers different pictures in different markets (for example `de-DE`, `ja-JP`, `en-GB`). With `autostart on`, a market other than `en-US` is saved in the startup command.

The program is built for the Windows GUI subsystem, so it has no console of its own. It attaches to the console it was started from. Some shells do not wait for GUI programs; if the prompt comes back before the output, run it through a pipe, for example `bingwall list | more`.

A command-line run and a running tray share the same files. You can use both.

## Where things are stored

`%LOCALAPPDATA%\bingwall\` (set the environment variable `BINGWALL_DIR` to use another folder):

- `images\<YYYYMMDD>_<first 8 characters of Bing's hsh>.jpg`: the pictures.
- `state.txt`: a small text index with the date, hash and title of each picture, the current picture, and the date of the last automatic change.

The newest 30 pictures are kept; older ones are deleted. The current picture is never deleted. Pictures are about 3.6 MB each, so the folder stays near 110 MB.

bingwall does not change your wallpaper fit or style settings. It only sets the wallpaper file, like the Settings app does.

## How the images are fetched

1. `GET https://www.bing.com/HPImageArchive.aspx?format=js&idx=0&n=8&mkt=en-US` lists the last 8 images (Bing returns at most 8 per call). On a first run, a second request with `idx=8` adds 8 older ones, so Next and Previous have about 16 pictures to walk.
2. The full-size picture is `https://www.bing.com` + `urlbase` + `_UHD.jpg`. If Bing has no UHD file, `_1920x1080.jpg` is used.
3. Each file is saved as `.part` first and renamed when complete, so a crash never leaves a half picture. The reply must be a real JPEG.
4. An image is known by its start date plus its `hsh`, so nothing is downloaded twice.

All requests use HTTPS, with a 10 second connect timeout and a 60 second limit for one picture.

## Uninstall

```
bingwall autostart off
```

Quit the tray icon, then delete `bingwall.exe` and the folder `%LOCALAPPDATA%\bingwall`. Your current wallpaper stays until you change it. (If you delete the folder while a bingwall picture is your wallpaper, pick another wallpaper in Settings.)

## Pictures

The pictures belong to Bing and to their photographers. bingwall only downloads them to set your own personal wallpaper. It does not host, copy or redistribute them. Do not use them for anything else without the rights holder's permission.

## Limits

- Windows 10 or 11, 64-bit.
- It uses the proxy settings that WinHTTP finds automatically. A proxy set only in the old Internet Options dialog may not be used.
- Slideshow wallpaper mode in Windows Settings replaces a picture bingwall set. Use "Picture" as the background type.

## License

MIT. See `LICENSE`.
