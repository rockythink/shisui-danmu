pub mod bilibili;
pub mod bridge;
#[cfg(feature = "tui")]
pub mod config;
pub mod delivery;
pub mod domain;
pub(crate) mod history;
pub(crate) mod history_repair;
mod legacy;
pub mod obs;
pub mod persistence;
pub(crate) mod runner;
pub mod runtime;
pub mod storage;
#[cfg(feature = "tui")]
pub mod terminal;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(feature = "tui")]
pub mod theme;
pub(crate) mod workspace;
pub(crate) mod workspace_config;
#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!("test-support is only available in debug builds");
