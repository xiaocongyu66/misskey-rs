//! Business services ported from `packages/backend/src/core/`:
//! NoteCreate, entity packers, MetaService, FanoutTimeline,
//! RoleService/policies, Drive, charts… (populated from M1/M2 on).
//! Must stay independent of axum so queue workers can reuse it.

pub mod packers;
pub mod services;
// push lane: social.rs + the tests tail address this as
// mk_core::push_deliver (the services module is otherwise the only path)
pub use services::push_deliver;
