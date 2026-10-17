//! Field-presence discipline for packed JSON responses.
//!
//! Misskey's packed schemas distinguish, per field:
//!   - `nullable: true, optional: false` → field ALWAYS present, may be null
//!     → [`Nullable<T>`] (plain `Option`, serialized as null)
//!   - `optional: true`                  → field may be ABSENT
//!     → [`Omit<T>`] (skipped when `None`)
//! e2e tests assert exact field sets (deepStrictEqual), so mixing these up
//! breaks compatibility in both directions.

use serde::Serialize;

/// Always-serialized field that may hold `null`.
pub type Nullable<T> = Option<T>;

/// Field omitted from the JSON object entirely when `None`.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct Omit<T>(#[serde(skip_serializing_if = "Option::is_none")] pub Option<T>);

impl<T> From<T> for Omit<T> {
    fn from(v: T) -> Self {
        Omit(Some(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    struct Sample {
        pub always: Nullable<String>,
        #[serde(rename = "maybeCamel")]
        pub maybe: Omit<String>,
    }

    #[test]
    fn nullable_stays_null_and_omit_vanishes() {
        let s = Sample {
            always: None,
            maybe: Omit(None),
        };
        assert_eq!(serde_json::to_value(&s).unwrap(), json!({ "always": null }));
        let s = Sample {
            always: Some("x".into()),
            maybe: Omit(Some("y".into())),
        };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({ "always": "x", "maybeCamel": "y" })
        );
    }
}
