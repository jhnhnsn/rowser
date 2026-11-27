//! rowser: open URLs from SSH sessions in the local browser.

pub mod browsers;
pub mod callback;
pub mod cli;
pub mod config;
pub mod control;
pub mod daemon;
pub mod detect;
pub mod discovery;
pub mod doctor;
pub mod host;
mod liveness;
pub mod logging;
pub mod mux;
pub mod outpost;
pub mod platform;
pub mod protocol;
pub mod remote_install;
pub mod route;
pub mod routes;
pub mod service;
pub mod shim;
#[cfg(test)]
mod test_support;
