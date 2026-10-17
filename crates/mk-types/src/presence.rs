//! Field-presence discipline for packed JSON responses.
//!
//! Misskey's packed schemas distinguish, per field:
//!   - `nullable: true, optional: false` → field ALWAYS present, may be null
//!     → [`Nullable<T>`] (plain `Option`, serializes as null)
//!   - `optional: true`                  → field may be ABSENT
//!     → plain `Option<T>` + `#[serde(skip_serializing_if = "Option::is_none")]`
//!
//! e2e tests assert exact field sets (deepStrictEqual), so mixing these up
//! breaks compatibility in both directions.
//!
//! NOTE: a newtype `Omit<T>` with `#[serde(transparent)]` + `skip_
//! serializing_if` does NOT work — `transparent` makes serde ignore the skip
//! attr and serialize `null`. That is why absence is expressed per-field.

/// Always-serialized field that may hold `null`.
pub type Nullable<T> = Option<T>;

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde_json::json;

    #[derive(Serialize)]
    struct Sample {
        pub always: Nullable<String>,
        #[serde(rename = "maybeCamel", skip_serializing_if = "Option::is_none")]
        pub maybe: Option<String>,
    }

    #[test]
    fn nullable_stays_null_and_absent_field_skips() {
        let s = Sample {
            always: None,
            maybe: None,
        };
        assert_eq!(serde_json::to_value(&s).unwrap(), json!({ "always": null }));
        let s = Sample {
            always: Some("x".into()),
            maybe: Some("y".into()),
        };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({ "always": "x", "maybeCamel": "y" })
        );
    }

    #[test]
    fn skip_works_under_flatten() {
        #[derive(Serialize)]
        struct Inner {
            #[serde(skip_serializing_if = "Option::is_none")]
            pub gone: Option<String>,
            pub kept: Nullable<String>,
        }
        #[derive(Serialize)]
        struct Outer {
            #[serde(flatten)]
            pub inner: Inner,
        }
        let v = serde_json::to_value(&Outer {
            inner: Inner {
                gone: None,
                kept: None,
            },
        })
        .unwrap();
        assert_eq!(v, json!({ "kept": null }));
        assert!(v.as_object().unwrap().get("gone").is_none());
    }
}
