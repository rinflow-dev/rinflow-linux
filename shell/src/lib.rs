//! What the Linux agent and window both need.
//!
//! Nothing here touches a window system. The agent folds core events into
//! a [`state::Snapshot`], the window renders it, and both speak the
//! [`protocol`] over a local socket.

pub mod arrangement;
pub mod i18n;
pub mod identity;
pub mod protocol;
pub mod settings;
pub mod state;
pub mod store;
pub mod trust;
