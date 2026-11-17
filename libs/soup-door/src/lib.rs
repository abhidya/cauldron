//! **soup-door**: a cauldron's soup, offline, as a library.
//!
//! Everything the command line's `cauldron soup` verbs do, without the
//! printing: [`marker`] and [`target`] find the soup a call is about,
//! [`reads`] answers `regions`, `region`, `nodes`, `search` and `get`,
//! [`import`] lands records all or nothing, [`recover`] settles a soup an
//! interrupted import left behind, and [`lock`] holds the two proofs every
//! writer makes first. [`budget`] bounds a whole reply, and [`grants`] is the
//! store of which cauldrons the owner opened to the MCP door; [`catalogue`]
//! is the list of tools that door answers.
//!
//! Every answer is a [`serde_json::Value`] (the JSON the CLI prints, byte
//! for byte) or an [`Error`] with a stable [`Code`] and the message a person
//! reads.

pub mod error;
pub mod budget;
pub mod catalogue;
pub mod cut;
pub mod dumps;
pub mod grants;
pub mod import;
pub mod lock;
pub mod marker;
pub mod reads;
pub mod recover;
pub mod target;

pub use error::{Code, Error, Result};
pub use target::Target;
