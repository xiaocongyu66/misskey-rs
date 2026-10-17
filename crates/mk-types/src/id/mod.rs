//! ID generation. Ports `packages/backend/src/misc/id/`.
//!
//! All timeline queries rely on `ORDER BY id DESC` meaning time-ordered, so
//! bytes generated here must sort lexicographically by time exactly like the
//! upstream generators. Default (and only) scheme for now: aid.

pub mod aid;

pub use aid::{gen_aid, gen_aid_now, parse_aid_date};
