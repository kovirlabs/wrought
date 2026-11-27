//! Wrought engine core: config, the render dial, routing, cache tags,
//! minijinja templates with built-in components, Markdown content, build
//! mode and the built-in static file server.
//!
//! Core implements the `build` render mode and the pieces the other modes
//! reuse: [`builder::Renderer`] renders any single route, and
//! [`builder::SiteExtension`] lets the `content` feature add pattern routes
//! and template functions. `publish` (engine-publish) and `request` parse
//! everywhere a mode can be set and fail validation with a "not in this
//! build" error when their features are off (see [`features`]).
//!
//! With the `request` marker feature core also holds the fragment cache
//! ([`fragments`]), the HTTP helpers for form posts ([`web`]) and the App UI
//! helpers ([`appui`]).

#[cfg(feature = "request")]
pub mod appui;
pub mod builder;
#[cfg(test)]
mod component_tests;
pub mod components;
pub mod config;
pub mod content;
pub mod features;
pub mod fragments;
pub mod routes;
pub mod serve;
pub mod tags;
#[cfg(feature = "targets")]
pub mod target;
pub mod templates;
pub mod timings;
pub mod ui;
#[cfg(feature = "request")]
pub mod web;

pub use builder::{
    BuildOptions, BuildReport, DynamicPage, Renderer, SiteExtension, build_site, build_site_with,
};
pub use config::{RenderMode, SiteConfig};
