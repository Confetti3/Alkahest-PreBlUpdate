//! On-disk layouts unique to the Shadowkeep / Season of Arrivals client.
//!
//! These types are intentionally isolated from the later-era data modules:
//! similarly named resources have incompatible class IDs and field ownership.

pub mod entity;
pub mod geometry;
pub mod map;

pub use entity::*;
pub use geometry::*;
pub use map::*;
