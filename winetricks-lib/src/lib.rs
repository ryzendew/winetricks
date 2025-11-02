//! Winetricks Library
//!
//! Core library for managing Wine packages and DLLs.
//! Provides fast, modern implementation of winetricks functionality.

pub mod config;
pub mod download;
pub mod error;
pub mod executor;
pub mod installer;
pub mod verb;
pub mod wine;

pub use config::Config;
pub use error::{Result, WinetricksError};
pub use executor::Executor;
pub use verb::{MediaType, Verb, VerbCategory, VerbFile, VerbMetadata, VerbRegistry};
pub use wine::Wine;
