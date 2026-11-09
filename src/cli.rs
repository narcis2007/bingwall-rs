//! Command-line parsing. No arguments (or only `--market`) means tray mode.

use std::ffi::OsString;

use crate::bing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Tray,
    Today,
    Next,
    Prev,
    List,
    Autostart(bool),
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub market: String,
    pub command: Command,
}

pub const USAGE: &str = "\
bingwall - a tiny Bing daily wallpaper changer

usage: bingwall [--market CODE] [COMMAND]

  (no command)    run in the system tray
  today           download the newest images and set the newest one
  next            set the next older cached image
  prev            set the previous (newer) cached image
  list            list the cached images
  autostart on    start bingwall at logon
  autostart off   do not start bingwall at logon

  --market CODE   Bing market, for example en-US (default), de-DE, ja-JP
  --version       print the version";

pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Args, String> {
    let mut market = bing::DEFAULT_MARKET.to_string();
    let mut words: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let arg = arg.into_string().map_err(|_| "arguments must be valid Unicode".to_string())?;
        if arg == "--market" {
            let v = it.next().ok_or("--market needs a value, for example --market en-US")?;
            market = v.into_string().map_err(|_| "arguments must be valid Unicode".to_string())?;
        } else if let Some(v) = arg.strip_prefix("--market=") {
            market = v.to_string();
        } else {
            words.push(arg);
        }
    }
    if !bing::valid_market(&market) {
        return Err(format!("bad market code \"{market}\" (examples: en-US, de-DE)"));
    }
    let command = match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => Command::Tray,
        ["today"] => Command::Today,
        ["next"] => Command::Next,
        ["prev" | "previous"] => Command::Prev,
        ["list"] => Command::List,
        ["autostart", "on"] => Command::Autostart(true),
        ["autostart", "off"] => Command::Autostart(false),
        ["autostart"] => return Err("autostart needs \"on\" or \"off\"".to_string()),
        ["help" | "--help" | "-h" | "-?"] => Command::Help,
        ["--version" | "-V" | "version"] => Command::Version,
        other => return Err(format!("unknown command: {}", other.join(" "))),
    };
    Ok(Args { market, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(OsString::from))
    }

    fn ok(args: &[&str]) -> (Command, String) {
        let a = p(args).unwrap();
        (a.command, a.market)
    }

    #[test]
    fn commands() {
        assert_eq!(ok(&[]), (Command::Tray, "en-US".into()));
        assert_eq!(ok(&["today"]).0, Command::Today);
        assert_eq!(ok(&["next"]).0, Command::Next);
        assert_eq!(ok(&["prev"]).0, Command::Prev);
        assert_eq!(ok(&["previous"]).0, Command::Prev);
        assert_eq!(ok(&["list"]).0, Command::List);
        assert_eq!(ok(&["autostart", "on"]).0, Command::Autostart(true));
        assert_eq!(ok(&["autostart", "off"]).0, Command::Autostart(false));
        assert_eq!(ok(&["--help"]).0, Command::Help);
        assert_eq!(ok(&["--version"]).0, Command::Version);
    }

    #[test]
    fn market_anywhere() {
        assert_eq!(ok(&["--market", "de-DE", "today"]), (Command::Today, "de-DE".into()));
        assert_eq!(ok(&["today", "--market", "ja-JP"]), (Command::Today, "ja-JP".into()));
        assert_eq!(ok(&["--market=fr-FR"]), (Command::Tray, "fr-FR".into()));
        assert_eq!(ok(&["autostart", "--market", "de-DE", "on"]), (Command::Autostart(true), "de-DE".into()));
    }

    #[test]
    fn errors() {
        assert!(p(&["--market"]).is_err());
        assert!(p(&["--market", "en US", "today"]).is_err());
        assert!(p(&["--market", "x&y=1"]).is_err());
        assert!(p(&["autostart"]).is_err());
        assert!(p(&["autostart", "maybe"]).is_err());
        assert!(p(&["today", "next"]).is_err());
        assert!(p(&["dance"]).is_err());
    }
}
