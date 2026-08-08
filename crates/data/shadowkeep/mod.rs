//! On-disk layouts unique to the Shadowkeep / Season of Arrivals client.
//!
//! These types are intentionally isolated from the later-era data modules:
//! similarly named resources have incompatible class IDs and field ownership.

pub mod entity;
pub mod geometry;
pub mod light;
pub mod map;
pub mod texture;

pub use entity::*;
pub use geometry::*;
pub use light::*;
pub use map::*;
pub use texture::*;
