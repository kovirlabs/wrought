//! Wrought engine core: config, the render dial, routing, cache tags,
//! minijinja templates with built-in components, Markdown content, build
//! mode and the built-in static file server.
//!
//! Core implements the `build` render mode and the pieces the other modes
//! reuse: [`builder::Renderer`] renders any single route, and
//! [`builder::SiteExtension`] lets the `content` feature add pattern routes
//! and template functions. `publish` (engine-publish) and `request` (v0.4)
//! parse everywhere a mode can be set and fail validation with a "not in this
//! build" error when their features are off (see [`features`]).

pub mod builder;
pub mod config;
pub mod content;
pub mod features;
pub mod routes;
pub mod serve;
pub mod tags;
pub mod templates;
pub mod ui;

pub use builder::{
    BuildOptions, BuildReport, DynamicPage, Renderer, SiteExtension, build_site, build_site_with,
};
pub use config::{RenderMode, SiteConfig};
