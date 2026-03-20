//! Full-screen session TUI (transcript + streaming + composer).

pub mod app;
pub mod bridge;
pub mod orchestration;
pub mod state;

pub use app::{TuiCmd, run_blocking};
pub use bridge::spawn_tui_bridge;
pub use state::{DisplayBlock, TuiSessionState};
