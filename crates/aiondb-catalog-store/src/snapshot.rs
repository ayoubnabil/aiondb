//! Catalog snapshot persistence for checkpoint/recovery.
//!
//! The catalog state is serialized to JSON and written to disk with a checksum.
//!
//! ## File format
//!
//! ```text
//! MAGIC (9 bytes): b"AIONCAT1\0"    -- stable catalog format v1
//! checkpoint_lsn: u64 LE
//! json_len: u64 LE                  -- byte length of the JSON blob
//! <json blob>                       -- serde_json-serialized CatalogState
//! checksum: u32 LE                  -- CRC32C of everything before this field
//! ```

use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;

use aiondb_core::checksum::{compute_crc32c, compute_legacy_fnv1a};
use aiondb_core::{DbError, DbResult};
use aiondb_wal::Lsn;

use crate::{CatalogState, CatalogStore};

/// Magic bytes that identify a stable v1 catalog snapshot file.
const CATALOG_SNAPSHOT_MAGIC: &[u8; 9] = b"AIONCAT1\0";
/// Historical v0.1 catalog snapshot magic accepted for explicit upgrades.
const LEGACY_CATALOG_SNAPSHOT_MAGIC: &[u8; 9] = b"AION_CAT\x01";

/// Name of the catalog snapshot file inside the data directory.
const CATALOG_SNAPSHOT_FILENAME: &str = "catalog.snapshot";
/// Temporary filename used for atomic writes.
const CATALOG_SNAPSHOT_TMP: &str = "catalog.snapshot.tmp";
const DEFAULT_MAX_CATALOG_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
const MIN_MAX_CATALOG_SNAPSHOT_BYTES: u64 = 1024 * 1024;
const MAX_MAX_CATALOG_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;

fn parse_max_catalog_snapshot_bytes(value: Option<&str>) -> u64 {
    value.and_then(|raw| raw.parse::<u64>().ok()).map_or(
        DEFAULT_MAX_CATALOG_SNAPSHOT_BYTES,
        |bytes| {
            bytes.clamp(
                MIN_MAX_CATALOG_SNAPSHOT_BYTES,
                MAX_MAX_CATALOG_SNAPSHOT_BYTES,
            )
        },
    )
}

fn max_catalog_snapshot_bytes() -> u64 {
    static MAX_CATALOG_SNAPSHOT_BYTES: OnceLock<u64> = OnceLock::new();
    *MAX_CATALOG_SNAPSHOT_BYTES.get_or_init(|| {
        parse_max_catalog_snapshot_bytes(
            std::env::var("AIONDB_CATALOG_MAX_SNAPSHOT_BYTES")
                .ok()
                .as_deref(),
        )
    })
}

fn read_fixed<const N: usize>(data: &[u8], offset: usize, field: &str) -> DbResult<[u8; N]> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| DbError::internal(format!("catalog snapshot: {field} offset overflow")))?;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| DbError::internal(format!("catalog snapshot: {field} truncated")))?;
    let mut bytes = [0u8; N];
    bytes.copy_from_slice(slice);
    Ok(bytes)
}

/// Header returned when a catalog snapshot is read.
#[derive(Clone, Debug)]
pub struct CatalogSnapshotHeader {
    pub checkpoint_lsn: Lsn,
}

fn create_snapshot_tmp_file(path: &Path) -> DbResult<std::fs::File> {
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                std::fs::remove_file(path).map_err(|remove_error| {
                    DbError::internal(format!(
                        "catalog snapshot: stale temp cleanup failed {}: {remove_error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(DbError::internal(format!(
                    "catalog snapshot: create temp failed: {error}"
                )));
            }
        }
    }

    Err(DbError::internal(
        "catalog snapshot: failed to create temp file",
    ))
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Save the current `CatalogState` to a snapshot file in `dir`.
///
/// The write is atomic: write to temp, fsync, rename.
pub fn save_catalog_snapshot(
    state: &CatalogState,
    checkpoint_lsn: Lsn,
    dir: &Path,
) -> DbResult<CatalogSnapshotHeader> {
    let json_bytes = serde_json::to_vec(state)
        .map_err(|e| DbError::internal(format!("catalog snapshot: serialize failed: {e}")))?;

    let json_len = json_bytes.len() as u64;

    // Build file contents.
    let mut file_buf =
        Vec::with_capacity(CATALOG_SNAPSHOT_MAGIC.len() + 8 + 8 + json_bytes.len() + 4);
    file_buf.extend_from_slice(CATALOG_SNAPSHOT_MAGIC);
    file_buf.extend_from_slice(&checkpoint_lsn.get().to_le_bytes());
    file_buf.extend_from_slice(&json_len.to_le_bytes());
    file_buf.extend_from_slice(&json_bytes);
    let checksum = compute_crc32c(&file_buf);
    file_buf.extend_from_slice(&checksum.to_le_bytes());

    // Atomic write.
    std::fs::create_dir_all(dir)
        .map_err(|e| DbError::internal(format!("catalog snapshot: cannot create dir: {e}")))?;

    let tmp_path = dir.join(CATALOG_SNAPSHOT_TMP);
    let final_path = dir.join(CATALOG_SNAPSHOT_FILENAME);

    let mut file = create_snapshot_tmp_file(&tmp_path)?;
    file.write_all(&file_buf)
        .map_err(|e| DbError::internal(format!("catalog snapshot: write failed: {e}")))?;
    file.flush()
        .map_err(|e| DbError::internal(format!("catalog snapshot: flush failed: {e}")))?;
    file.sync_all()
        .map_err(|e| DbError::internal(format!("catalog snapshot: fsync failed: {e}")))?;
    drop(file);

    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| DbError::internal(format!("catalog snapshot: rename failed: {e}")))?;

    // Fsync the directory to make the rename durable across crashes.
    aiondb_wal::segment::sync_dir(dir)?;

    Ok(CatalogSnapshotHeader { checkpoint_lsn })
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Load a catalog snapshot from `dir`. Returns `None` if no file exists.
pub fn load_catalog_snapshot(
    dir: &Path,
) -> DbResult<Option<(CatalogSnapshotHeader, CatalogState)>> {
    let path = dir.join(CATALOG_SNAPSHOT_FILENAME);
    let file_len = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(DbError::internal(format!(
                "catalog snapshot: metadata failed: {e}"
            )));
        }
    };
    let max_snapshot_bytes = max_catalog_snapshot_bytes();
    if file_len > max_snapshot_bytes {
        return Err(DbError::internal(format!(
            "catalog snapshot: file size {file_len} exceeds maximum {max_snapshot_bytes} bytes"
        )));
    }
    let file = std::fs::File::open(&path)
        .map_err(|e| DbError::internal(format!("catalog snapshot: read failed: {e}")))?;
    let mut data = Vec::with_capacity(usize::try_from(file_len).unwrap_or(0));
    let mut limited = file.take(max_snapshot_bytes.saturating_add(1));
    limited
        .read_to_end(&mut data)
        .map_err(|e| DbError::internal(format!("catalog snapshot: read failed: {e}")))?;
    let loaded_len = u64::try_from(data.len()).unwrap_or(u64::MAX);
    if loaded_len > max_snapshot_bytes {
        return Err(DbError::internal(format!(
            "catalog snapshot: file grew to {loaded_len} bytes while reading and exceeds maximum {max_snapshot_bytes} bytes"
        )));
    }

    // Minimum: magic(9) + lsn(8) + json_len(8) + checksum(4) = 29
    if data.len() < 29 {
        return Err(DbError::internal("catalog snapshot: file too small"));
    }

    // Verify magic.
    if &data[..9] != CATALOG_SNAPSHOT_MAGIC && &data[..9] != LEGACY_CATALOG_SNAPSHOT_MAGIC {
        return Err(DbError::internal("catalog snapshot: invalid magic bytes"));
    }

    // Verify checksum.
    let checksum_offset = data.len() - 4;
    let stored_checksum = u32::from_le_bytes(read_fixed(&data, checksum_offset, "checksum")?);
    let computed_checksum = compute_crc32c(&data[..checksum_offset]);
    if stored_checksum != computed_checksum
        && stored_checksum != compute_legacy_fnv1a(&data[..checksum_offset])
    {
        return Err(DbError::internal("catalog snapshot: checksum mismatch"));
    }

    // Parse header fields.
    let mut offset = 9;
    let checkpoint_lsn = Lsn::new(u64::from_le_bytes(read_fixed(
        &data,
        offset,
        "checkpoint LSN",
    )?));
    offset += 8;

    let json_len_u64 = u64::from_le_bytes(read_fixed(&data, offset, "JSON length")?);
    let json_len = usize::try_from(json_len_u64)
        .map_err(|_| DbError::internal("catalog snapshot: JSON length overflow"))?;
    offset += 8;

    let expected_file_len = offset
        .checked_add(json_len)
        .and_then(|s| s.checked_add(4))
        .ok_or_else(|| DbError::internal("catalog snapshot: JSON length overflow"))?;
    if expected_file_len > data.len() {
        return Err(DbError::internal("catalog snapshot: JSON blob truncated"));
    }
    if expected_file_len != data.len() {
        return Err(DbError::internal(
            "catalog snapshot: trailing bytes after JSON blob",
        ));
    }

    let json_blob = &data[offset..offset + json_len];
    let mut state: CatalogState = serde_json::from_slice(json_blob)
        .map_err(|e| DbError::internal(format!("catalog snapshot: deserialize failed: {e}")))?;
    CatalogStore::canonicalize_privilege_list(&mut state.privileges);

    Ok(Some((CatalogSnapshotHeader { checkpoint_lsn }, state)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        crate::test_support::unique_temp_path("snapshot-test", name)
    }

    #[test]
    fn snapshot_roundtrip_default_state() {
        let dir = test_dir("roundtrip_default");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        let lsn = Lsn::new(42);

        let header = save_catalog_snapshot(&state, lsn, &dir).unwrap();
        assert_eq!(header.checkpoint_lsn, lsn);

        let (loaded_header, loaded_state) = load_catalog_snapshot(&dir).unwrap().unwrap();
        assert_eq!(loaded_header.checkpoint_lsn, lsn);
        // Schema count should match
        assert_eq!(state.schemas_by_id.len(), loaded_state.schemas_by_id.len());
        assert_eq!(state.schema_names.len(), loaded_state.schema_names.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_roundtrip_with_tables_and_roles() {
        use aiondb_catalog::{
            ColumnDescriptor, QualifiedName, RoleDescriptor, SchemaDescriptor, TableDescriptor,
        };
        use aiondb_core::{ColumnId, DataType, RelationId, SchemaId};

        let dir = test_dir("roundtrip_complex");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);

        // Add a custom schema
        let sid = SchemaId::new(state.next_schema_id);
        state.next_schema_id += 1;
        state.schemas_by_id.insert(
            sid,
            SchemaDescriptor {
                schema_id: sid,
                name: "myschema".to_owned(),
            },
        );
        state.schema_names.insert("myschema".to_owned(), sid);

        // Add a table
        let tid = RelationId::new(state.next_table_id);
        state.next_table_id += 1;
        let table = TableDescriptor {
            table_id: tid,
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "users"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        state
            .table_names
            .insert((SchemaId::new(1), "users".to_owned()), tid);
        state.tables_by_id.insert(tid, table);

        // Add a role
        state.roles.insert(
            "admin".to_owned(),
            RoleDescriptor {
                name: "admin".to_owned(),
                login: true,
                superuser: true,
                password_hash: Some("hashed".to_owned()),
                ..RoleDescriptor::default()
            },
        );

        let lsn = Lsn::new(100);
        save_catalog_snapshot(&state, lsn, &dir).unwrap();

        let (header, loaded) = load_catalog_snapshot(&dir).unwrap().unwrap();
        assert_eq!(header.checkpoint_lsn, Lsn::new(100));
        assert_eq!(loaded.tables_by_id.len(), 1);
        assert!(loaded.tables_by_id.contains_key(&tid));
        assert!(loaded.roles.contains_key("admin"));
        assert!(loaded.schema_names.contains_key("myschema"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_roundtrip_persists_pg_object_descriptors() {
        // Disk-snapshot regression for the descriptors added by the
        // CREATE TYPE / DOMAIN / CAST / POLICY / RULE persistence work.
        // Tags 71..=83 round-trip through the WAL codec, but the snapshot
        // path is a separate code path (serde_json on the whole
        // `CatalogState`) - a snapshot dropped before WAL replay must
        // still surface the in-memory state on the next boot.
        use aiondb_catalog::{
            CastContextDescriptor, CastDescriptor, CastMethodDescriptor,
            DomainConstraintDescriptor, DomainDescriptor, PolicyCommandDescriptor,
            PolicyDescriptor, PolicyKindDescriptor, RuleDescriptor, RuleEventDescriptor,
            UserTypeDescriptor, UserTypeFieldDescriptor,
        };
        use aiondb_core::DataType;

        let dir = test_dir("pg_object_snapshot");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);

        state.domains.insert(
            "positive_int".to_owned(),
            DomainDescriptor {
                name: "positive_int".to_owned(),
                schema_name: None,
                base_type: "int4".to_owned(),
                not_null: true,
                default_expr: None,
                constraints: vec![DomainConstraintDescriptor {
                    name: "positive_int_check".to_owned(),
                    check_expr: "VALUE > 0".to_owned(),
                }],
                char_length: None,
                owner: Some("aiondb".to_owned()),
            },
        );
        state.user_types.insert(
            "mood".to_owned(),
            UserTypeDescriptor {
                name: "mood".to_owned(),
                schema_name: None,
                oid: 120_001,
                enum_labels: vec!["sad".to_owned(), "ok".to_owned(), "happy".to_owned()],
                composite_fields: Vec::new(),
                owner: None,
            },
        );
        state.user_types.insert(
            "point2d".to_owned(),
            UserTypeDescriptor {
                name: "point2d".to_owned(),
                schema_name: None,
                oid: 120_002,
                enum_labels: Vec::new(),
                composite_fields: vec![
                    UserTypeFieldDescriptor {
                        name: "x".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: Some("int4".to_owned()),
                    },
                    UserTypeFieldDescriptor {
                        name: "y".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: Some("int4".to_owned()),
                    },
                ],
                owner: None,
            },
        );
        state.casts.insert(
            ("int4".to_owned(), "text".to_owned()),
            CastDescriptor {
                oid: 121_001,
                source_type: "int4".to_owned(),
                target_type: "text".to_owned(),
                context: CastContextDescriptor::Assignment,
                method: CastMethodDescriptor::Function {
                    function_name: "int4_to_text_double".to_owned(),
                    function_oid: 122_001,
                },
                owner: Some("aiondb".to_owned()),
            },
        );
        state.policies.insert(
            ("rls_persist_p".to_owned(), "rls_persist".to_owned()),
            PolicyDescriptor {
                name: "rls_persist_p".to_owned(),
                table_name: "rls_persist".to_owned(),
                command: PolicyCommandDescriptor::Select,
                kind: PolicyKindDescriptor::Permissive,
                roles: vec!["bob".to_owned()],
                using_expr: Some("owner = current_user".to_owned()),
                with_check_expr: None,
                owner: Some("aiondb".to_owned()),
            },
        );
        state.rules.insert(
            ("block_persist".to_owned(), "rule_persist".to_owned()),
            RuleDescriptor {
                name: "block_persist".to_owned(),
                table_name: "rule_persist".to_owned(),
                event: RuleEventDescriptor::Delete,
                is_instead: true,
                action_sql: "NOTHING".to_owned(),
                returning_count: 0,
                owner: None,
            },
        );

        let lsn = Lsn::new(7);
        save_catalog_snapshot(&state, lsn, &dir).unwrap();
        let (header, loaded) = load_catalog_snapshot(&dir).unwrap().unwrap();
        assert_eq!(header.checkpoint_lsn, lsn);

        assert_eq!(loaded.domains.len(), 1);
        let domain = loaded.domains.get("positive_int").expect("domain");
        assert_eq!(domain.constraints.len(), 1);
        assert!(domain.not_null);

        assert_eq!(loaded.user_types.len(), 2);
        assert_eq!(
            loaded.user_types.get("mood").unwrap().enum_labels,
            vec!["sad", "ok", "happy"]
        );
        assert_eq!(
            loaded
                .user_types
                .get("point2d")
                .unwrap()
                .composite_fields
                .len(),
            2
        );

        assert_eq!(loaded.casts.len(), 1);
        let cast = loaded
            .casts
            .get(&("int4".to_owned(), "text".to_owned()))
            .expect("cast");
        match &cast.method {
            CastMethodDescriptor::Function { function_name, .. } => {
                assert_eq!(function_name, "int4_to_text_double");
            }
            other => panic!("unexpected cast method {other:?}"),
        }

        assert_eq!(loaded.policies.len(), 1);
        let policy = loaded
            .policies
            .get(&("rls_persist_p".to_owned(), "rls_persist".to_owned()))
            .expect("policy");
        assert_eq!(policy.using_expr.as_deref(), Some("owner = current_user"));

        assert_eq!(loaded.rules.len(), 1);
        let rule = loaded
            .rules
            .get(&("block_persist".to_owned(), "rule_persist".to_owned()))
            .expect("rule");
        assert!(rule.is_instead);
        assert!(matches!(rule.event, RuleEventDescriptor::Delete));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_no_file_returns_none() {
        let dir = test_dir("no_file");
        std::fs::create_dir_all(&dir).unwrap();
        let result = load_catalog_snapshot(&dir).unwrap();
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_corrupt_checksum_fails() {
        let dir = test_dir("corrupt_checksum");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        save_catalog_snapshot(&state, Lsn::new(1), &dir).unwrap();

        let path = dir.join(CATALOG_SNAPSHOT_FILENAME);
        let mut data = std::fs::read(&path).unwrap();
        let mid = data.len() / 2;
        data[mid] ^= 0xFF;
        std::fs::write(&path, &data).unwrap();

        let result = load_catalog_snapshot(&dir);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_truncated_json_length_fails_cleanly() {
        let dir = test_dir("truncated_json_length");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        save_catalog_snapshot(&state, Lsn::new(1), &dir).unwrap();

        let path = dir.join(CATALOG_SNAPSHOT_FILENAME);
        let mut data = std::fs::read(&path).unwrap();
        let json_len_offset = 9 + 8;
        data[json_len_offset..json_len_offset + 8].copy_from_slice(&(u64::MAX / 2).to_le_bytes());
        let checksum_offset = data.len() - 4;
        let checksum = compute_crc32c(&data[..checksum_offset]);
        data[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&path, &data).unwrap();

        let err = load_catalog_snapshot(&dir).expect_err("truncated catalog snapshot must fail");
        assert!(err.to_string().contains("JSON blob truncated"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_rejects_trailing_bytes_after_json() {
        let dir = test_dir("trailing_bytes");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        save_catalog_snapshot(&state, Lsn::new(1), &dir).unwrap();

        let path = dir.join(CATALOG_SNAPSHOT_FILENAME);
        let data = std::fs::read(&path).unwrap();
        let checksum_offset = data.len() - 4;
        let mut extended = Vec::new();
        extended.extend_from_slice(&data[..checksum_offset]);
        extended.extend_from_slice(b"extra");
        let checksum = compute_crc32c(&extended);
        extended.extend_from_slice(&checksum.to_le_bytes());
        std::fs::write(&path, &extended).unwrap();

        let err = load_catalog_snapshot(&dir).expect_err("trailing bytes should fail");
        assert!(err.to_string().contains("trailing bytes"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_load_accepts_fnv_checksum() {
        let dir = test_dir("fnv_checksum");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        save_catalog_snapshot(&state, Lsn::new(12), &dir).unwrap();

        let path = dir.join(CATALOG_SNAPSHOT_FILENAME);
        let mut data = std::fs::read(&path).unwrap();
        let checksum_offset = data.len() - 4;
        let fnv_checksum = compute_legacy_fnv1a(&data[..checksum_offset]);
        data[checksum_offset..].copy_from_slice(&fnv_checksum.to_le_bytes());
        std::fs::write(&path, &data).unwrap();

        let loaded = load_catalog_snapshot(&dir).expect("historical checksum should still load");
        assert!(loaded.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_overwrite_replaces_old() {
        let dir = test_dir("overwrite");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        save_catalog_snapshot(&state, Lsn::new(10), &dir).unwrap();

        state.roles.insert(
            "newrole".to_owned(),
            aiondb_catalog::RoleDescriptor {
                name: "newrole".to_owned(),
                login: false,
                superuser: false,
                password_hash: None,
                ..aiondb_catalog::RoleDescriptor::default()
            },
        );
        save_catalog_snapshot(&state, Lsn::new(20), &dir).unwrap();

        let (header, loaded) = load_catalog_snapshot(&dir).unwrap().unwrap();
        assert_eq!(header.checkpoint_lsn, Lsn::new(20));
        assert!(loaded.roles.contains_key("newrole"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_load_migrates_function_execute_privileges() {
        use aiondb_catalog::{
            CatalogPrivilege, FunctionPrivilegeTarget, PrivilegeDescriptor, PrivilegeTarget,
            QualifiedName, RoleDescriptor,
        };

        let dir = test_dir("function_privilege_migration");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        state.roles.insert(
            "reader".to_owned(),
            RoleDescriptor {
                name: "reader".to_owned(),
                login: true,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        );
        state.privileges.push(PrivilegeDescriptor {
            role_name: "reader".to_owned(),
            privilege: CatalogPrivilege::Execute,
            target: PrivilegeTarget::Table(QualifiedName::qualified("public", "function_v1")),
        });

        save_catalog_snapshot(&state, Lsn::new(55), &dir).unwrap();
        let (_, loaded) = load_catalog_snapshot(&dir).unwrap().unwrap();

        assert_eq!(
            loaded.privileges,
            vec![PrivilegeDescriptor {
                role_name: "reader".to_owned(),
                privilege: CatalogPrivilege::Execute,
                target: PrivilegeTarget::Function(FunctionPrivilegeTarget {
                    name: QualifiedName::qualified("public", "function_v1"),
                    arg_types: None,
                }),
            }]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
