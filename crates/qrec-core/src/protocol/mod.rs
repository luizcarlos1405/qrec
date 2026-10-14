//! The dispatch protocol (A§4): commands and queries in; effects, replies,
//! progress and toasts out; events back in. Every type is plain data with
//! `Debug, Clone, PartialEq, Serialize, Deserialize`.

pub mod command;
pub mod effect;
pub mod event;
pub mod progress;
pub mod query;
pub mod reply;
pub mod toast;
pub mod wire;

pub use command::*;
pub use effect::*;
pub use event::*;
pub use progress::*;
pub use query::*;
pub use reply::*;
pub use toast::*;
pub use wire::*;
