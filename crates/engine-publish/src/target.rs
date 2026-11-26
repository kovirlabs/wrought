//! Publish targets: re-exported from core (`engine_core::target`), where
//! the bucket target of the `s3` feature can reach them without the
//! database crates.

pub use engine_core::target::{DiskTarget, PublishTarget, check_rel};
