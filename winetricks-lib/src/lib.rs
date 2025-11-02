//! Winetricks Library
//! 
//! Core library for managing Wine packages and DLLs.
//! Provides fast, modern implementation of winetricks functionality.

pub mod config;
pub mod wine;
pub mod verb;
pub mod download;
pub mod error;
pub mod executor;

pub use config::Config;
pub use wine::Wine;
pub use verb::{Verb, VerbMetadata, VerbCategory, VerbRegistry, MediaType, VerbFile};
pub use error::{WinetricksError, Result};
pub use executor::Executor;

