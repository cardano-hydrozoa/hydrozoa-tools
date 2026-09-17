//! Copy a Hydrozoa head's store out from under a running node, so the node can trim it.
//!
//! The archiver opens the node's own RocksDB directory as a **secondary** — a read-only handle
//! that catches up from the primary's WAL and manifest on demand. There is no socket, no IPC and
//! no coordination: the node keeps writing throughout and is never told this reader exists. Run it
//! as a user with `r-x` on the store directory and read-only stops being a promise.
//!
//! The archive is a second RocksDB in the same layout, so restoring is copying it back rather than
//! converting it, and values are written exactly as the node stored them — arrival-stamp prefix
//! and all.
//!
//! Once a pass is durably fsynced, the archiver tells the node how far it reached. That report is
//! the only thing it ever says to hydrozoa, and the only reason the node may delete anything. With
//! no node configured it archives and reports nothing, which leaves the node retaining everything —
//! the safe direction.

pub mod archive;
pub mod config;
pub mod pass;
pub mod watermark;

pub use archive::ArchiveStore;
pub use pass::{Gap, PassReport};
pub use watermark::{WatermarkReport, WatermarkResponse};
