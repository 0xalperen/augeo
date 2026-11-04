//! The terminal interface. Everything works by keyboard, by mouse and by command.
//!
//! [`App`] holds all state and takes events, so tests can drive it without a terminal. Anything that
//! reaches outside the engine, such as elevating or restarting, goes through [`Host`].

#![forbid(unsafe_code)]

mod app;
mod bench;
mod view;

pub use app::{App, Flow, Host, Reopen};

#[cfg(test)]
mod tests;
