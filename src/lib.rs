mod api;
mod auth;
mod covers;
mod desktop;
mod equalizer;
pub mod mcp;
mod media;
#[cfg(windows)]
mod media_session;
mod player;
mod preferences;
mod rpc;
#[cfg(windows)]
mod software_window;
pub mod stream;
pub mod ui;
#[cfg(windows)]
pub mod updater;
mod worker;
