//! bingwall: a tiny Bing daily wallpaper changer for Windows.
//!
//! `bing`, `json`, `store` and `cli` are pure logic and have unit tests.
//! `app` and `tray` join them with Windows. All `unsafe` code is in `win`.

#![deny(unsafe_code)]

pub mod app;
pub mod bing;
pub mod cli;
pub mod json;
pub mod store;
pub mod tray;
#[allow(unsafe_code)]
pub mod win;
