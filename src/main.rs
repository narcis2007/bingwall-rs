// No console window in tray mode. Command-line use attaches to the parent console.
#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::io::Write;

use bingwall::app::{self, Ctx};
use bingwall::bing;
use bingwall::cli::{self, Command};
use bingwall::{store, tray, win};

fn main() {
    std::process::exit(run());
}

/// Print to standard output. A closed or missing console is not an error.
fn say(text: &str) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn complain(text: &str) {
    let _ = writeln!(std::io::stderr(), "bingwall: {text}");
}

fn run() -> i32 {
    let args = match cli::parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            win::attach_console();
            complain(&e);
            complain("run \"bingwall --help\" for the list of commands");
            return 2;
        }
    };
    if args.command != Command::Tray {
        win::attach_console();
    }
    let market = args.market;
    match args.command {
        Command::Help => {
            say(cli::USAGE);
            0
        }
        Command::Version => {
            say(concat!("bingwall ", env!("CARGO_PKG_VERSION")));
            0
        }
        Command::Autostart(on) => autostart(on, &market),
        Command::Tray => with_ctx(market, tray::run),
        Command::Today => with_ctx(market, |c| today(&c)),
        Command::Next => with_ctx(market, |c| step(&c, 1)),
        Command::Prev => with_ctx(market, |c| step(&c, -1)),
        Command::List => with_ctx(market, |c| list(&c)),
    }
}

fn with_ctx(market: String, f: impl FnOnce(Ctx) -> i32) -> i32 {
    match Ctx::new(market) {
        Ok(ctx) => f(ctx),
        Err(e) => {
            complain(&e);
            1
        }
    }
}

fn describe(e: &store::Entry) -> String {
    format!("{}  {}", store::pretty_date(&e.date), e.title)
}

fn today(ctx: &Ctx) -> i32 {
    match app::today(ctx) {
        Ok((e, problem)) => {
            say(&format!("Wallpaper set: {}", describe(&e)));
            if let Some(p) = problem {
                complain(&format!("could not update from Bing ({p}); used a cached image"));
            }
            0
        }
        Err(e) => {
            complain(&e);
            1
        }
    }
}

fn step(ctx: &Ctx, delta: isize) -> i32 {
    match app::step(ctx, delta) {
        Ok(e) => {
            say(&format!("Wallpaper set: {}", describe(&e)));
            0
        }
        Err(e) => {
            complain(&e);
            1
        }
    }
}

fn list(ctx: &Ctx) -> i32 {
    let lines = app::list(ctx);
    if lines.is_empty() {
        say("No images cached. Run: bingwall today");
    }
    for line in lines {
        say(&line);
    }
    0
}

fn autostart(on: bool, market: &str) -> i32 {
    let result = if on {
        std::env::current_exe()
            .map_err(|e| format!("cannot find the program path: {e}"))
            .and_then(|exe| {
                let cmd = win::autostart_command(&exe, market, bing::DEFAULT_MARKET);
                win::autostart_set(&cmd).map(|()| format!("Autostart on: {}", cmd.to_string_lossy()))
            })
    } else {
        win::autostart_remove().map(|was_on| {
            if was_on { "Autostart off" } else { "Autostart was already off" }.to_string()
        })
    };
    match result {
        Ok(line) => {
            say(&line);
            0
        }
        Err(e) => {
            complain(&e);
            1
        }
    }
}
