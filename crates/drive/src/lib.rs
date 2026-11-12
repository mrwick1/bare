//! The test driver (S5 topics 2 and 3). It runs a journey of user steps (`steps.rs`) against a page through the control interface
//! (`bare::control`): the same hit testing, input events, timers and network as `bare --window`. It finds elements as a user names them
//! (`locate.rs`), waits for conditions with a time limit, and gives a JSON result with the console, the network requests and captures,
//! with the secrets hidden (`run.rs`). The binary is `bare-drive`.

pub mod diag;
pub mod locate;
pub mod run;
pub mod snap;
pub mod state;
pub mod steps;
pub use bare::{DownloadState, Downloads};
