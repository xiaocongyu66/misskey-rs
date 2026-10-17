//! Entity packers, ported from `src/core/entities/`. Response-shape structs
//! live here; the async pack functions that read the DB join in M1/M2.

pub mod note;
pub mod user;

pub use user::{MeDetailed, UserDetailedNotMe, UserLite};
