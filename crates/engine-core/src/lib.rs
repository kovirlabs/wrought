//! Wrought engine core: config, the render dial, routing, cache tags,
//! minijinja templates with built-in components, Markdown content, build
//! mode and the built-in static file server.
//!
//! v0.1 ("static") implements the `build` render mode. `publish` and
//! `request` parse everywhere a mode can be set and fail validation with a
//! "not in this build" error until their features land (see [`features`]).

pub mod builder;
pub mod config;
pub mod content;
pub mod features;
pub mod routes;
pub mod serve;
pub mod tags;
pub mod templates;
pub mod ui;

pub use builder::{BuildOptions, BuildReport, build_site};
pub use config::{RenderMode, SiteConfig};
