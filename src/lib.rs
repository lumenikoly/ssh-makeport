#![forbid(unsafe_code)]

pub mod engine;
pub mod log;
pub mod model;
pub mod platform;
pub mod process;
pub mod shell;
pub mod ssh_config;
pub mod store;

pub type Result<T> = std::result::Result<T, String>;
