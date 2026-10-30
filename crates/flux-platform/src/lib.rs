//! What differs between the systems Flux runs on, in one place (part 10.9): where Flux keeps its
//! files ([`paths`]), how paths are spelled and compared ([`path_style`]), how it starts and finds
//! other programs ([`process`]), the local time ([`time`]). Flux grew up on macOS; Windows came
//! next, Linux will follow. Code for one system only stays where it is used, behind
//! `cfg(target_os = …)`; what several crates need is here.

pub mod path_style;
pub mod paths;
pub mod process;
pub mod time;

pub use path_style::{PathStyle, join_slashed, same_path};
