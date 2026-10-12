//! Serde helpers shared by patch types.
//!
//! `Option<Option<T>>` is used for "leave alone" (outer `None`), "clear" (`Some(None)`) and "set"
//! (`Some(Some(v))`). Plain serde maps JSON `null` to the OUTER `None`, which loses "clear". Use
//! `#[serde(default, with = "crate::serde_util::double_option")]` on such fields: an absent key is
//! "leave alone", `null` is "clear", a value is "set". On serialisation the outer `None` must be
//! skipped with `skip_serializing_if = "Option::is_none"`.

/// (De)serialisation for `Option<Option<T>>` fields.
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Absent (handled by `default`) is outer `None`; `null` becomes `Some(None)`.
    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }

    /// Writes the inner option (outer `None` is skipped by the field attribute).
    pub fn serialize<T, S>(v: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error>
    where
        T: Serialize,
        S: Serializer,
    {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }
}
