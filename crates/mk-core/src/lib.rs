//! Business services ported from `packages/backend/src/core/`:
//! NoteCreate, entity packers, MetaService, FanoutTimeline,
//! RoleService/policies, Drive, charts… (populated from M1/M2 on).
//! Must stay independent of axum so queue workers can reuse it.
