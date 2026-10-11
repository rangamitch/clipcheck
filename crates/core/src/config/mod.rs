//! Config module: the serde schema, defaults and file loading (`schema`), and the
//! validation and apply-class rules (`validate`).
pub mod schema;
pub mod validate;
pub mod vk;

pub use validate::MAX_CLIP_HOTKEYS;
