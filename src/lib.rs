// Copyright (c) 2018-2020  Brendan Molloy <brendan@bbqsrc.net>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! A library implementing the Cucumber testing framework for Rust, in Rust.

#![recursion_limit = "512"]
#![deny(rust_2018_idioms)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// Re-export Gherkin for the convenience of everybody
pub use gherkin;

#[macro_use]
mod macros;

mod cli;
mod collection;
mod cucumber;
pub mod event;
mod examples;
pub mod output;
mod regex;
pub(crate) mod runner;
mod steps;

#[cfg(feature = "macros")]
#[doc(hidden)]
pub mod private;

use async_trait::async_trait;
use std::panic::UnwindSafe;

pub use cucumber::Cucumber;
pub use examples::ExampleValues;
pub use runner::RunResult;
use std::any::Any;
pub use steps::Steps;

#[cfg(feature = "macros")]
#[cfg_attr(docsrs, doc(cfg(feature = "macros")))]
#[doc(inline)]
pub use self::private::WorldInit;
#[cfg(feature = "macros")]
#[cfg_attr(docsrs, doc(cfg(feature = "macros")))]
#[doc(inline)]
pub use cucumber_rust_codegen::{given, then, when, WorldInit};
#[cfg(feature = "macros")]
#[doc(hidden)]
pub use futures;

const TEST_SKIPPED: &str = "Cucumber: test skipped";

#[macro_export]
macro_rules! skip {
    () => {
        panic!("Cucumber: test skipped");
    };
}

/// The `World` trait represents shared user-defined state
/// for a cucumber run.
#[async_trait(?Send)]
pub trait World: Sized + UnwindSafe + 'static {
    type Error: std::error::Error;

    async fn new() -> Result<Self, Self::Error>;
}

/// During test runs, a `Cucumber` instance notifies its
/// associated `EventHandler` implementation about the
/// key occurrences in the test lifecycle.
///
/// User can replace the default `EventHandler` for a `Cucumber`
/// at construction time using `Cucumber::with_handler`.
pub trait EventHandler: 'static {
    fn handle_event(&mut self, event: event::CucumberEvent);
}

pub type PanicError = Box<(dyn Any + Send + 'static)>;
pub enum TestError {
    TimedOut,
    PanicError(PanicError),
}
