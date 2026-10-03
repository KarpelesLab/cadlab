//! Schema versioning and migrations.
//!
//! Files are loaded as untyped values first, migrated step by step up to
//! [`CURRENT_SCHEMA_VERSION`], then deserialized into the model.

use crate::model::ModelError;
use crate::model::raw::RawProject;

/// Schema version written by this build.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// A migration from version `n` to `n + 1`, at index `n - 1`.
type Migration = fn(&mut RawProject) -> Result<(), String>;

/// Migrations, in order. Entry `i` upgrades version `i + 1` to `i + 2`.
const MIGRATIONS: &[Migration] = &[];

const _: () = assert!(MIGRATIONS.len() as u32 + 1 == CURRENT_SCHEMA_VERSION);

/// Upgrades `raw` in place to the current version. Returns the version found.
pub fn migrate(raw: &mut RawProject) -> Result<u32, ModelError> {
    migrate_with(raw, MIGRATIONS, CURRENT_SCHEMA_VERSION)
}

pub(crate) fn migrate_with(
    raw: &mut RawProject,
    migrations: &[Migration],
    current: u32,
) -> Result<u32, ModelError> {
    let found = raw.schema_version()?;
    if found > current {
        return Err(ModelError::NewerSchema {
            found,
            supported: current,
        });
    }
    if found == 0 {
        return Err(ModelError::invalid(
            "cadlab.toml",
            "schema_version must be >= 1",
        ));
    }
    for v in found..current {
        migrations[(v - 1) as usize](raw).map_err(|e| {
            ModelError::invalid(
                "cadlab.toml",
                format!("migration {v} -> {} failed: {e}", v + 1),
            )
        })?;
        raw.set_schema_version(v + 1);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(version: u32) -> RawProject {
        RawProject::from_manifest(json!({"schema_version": version, "name": "t"}))
    }

    #[test]
    fn runs_migrations_in_order() {
        fn m1(r: &mut RawProject) -> Result<(), String> {
            r.manifest["log"] = json!("1");
            Ok(())
        }
        fn m2(r: &mut RawProject) -> Result<(), String> {
            let prev = r.manifest["log"].as_str().unwrap_or("").to_string();
            r.manifest["log"] = json!(prev + ",2");
            Ok(())
        }
        let mut r = raw(1);
        assert_eq!(migrate_with(&mut r, &[m1, m2], 3).unwrap(), 1);
        assert_eq!(r.manifest["log"], "1,2");
        assert_eq!(r.schema_version().unwrap(), 3);

        let mut r = raw(2);
        r.manifest["log"] = json!("x");
        migrate_with(&mut r, &[m1, m2], 3).unwrap();
        assert_eq!(r.manifest["log"], "x,2");
    }

    #[test]
    fn rejects_newer() {
        let mut r = raw(CURRENT_SCHEMA_VERSION + 1);
        assert!(matches!(
            migrate(&mut r),
            Err(ModelError::NewerSchema { .. })
        ));
    }
}
