//! WAL codec helpers for catalog DDL records.
//!
//! All catalog descriptors are serialised to/from JSON blobs via `serde_json`.
//! The binary wire format for each catalog record is:
//!
//! ```text
//! tag (u8)               -- already written by the caller
//! txn_id (u64 LE)
//! [scalar fields]        -- variant-specific (e.g. schema_id_raw, role_name)
//! json_len (u32 LE)      -- only for variants carrying a descriptor_json
//! json_bytes (json_len)
//! ```

use super::binary_io::{BinaryReader, BinaryWriter};
use crate::record::WalRecord;
use aiondb_core::{DbError, DbResult, TxnId};

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

pub(super) fn write_catalog_payload(w: &mut BinaryWriter, record: &WalRecord) -> DbResult<()> {
    match record {
        WalRecord::CatalogCreateSchema {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateRole {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogAlterRole {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateView {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateSequence {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogAlterSequence {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateFunction {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateTrigger {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogGrantPrivilege {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogRevokePrivilege {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogSetTableDescriptor {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogSetIndexDescriptor {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateTenant {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogUpdateStatistics {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateNodeLabel {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateEdgeLabel {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateDomain {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogAlterDomain {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateUserType {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogAlterUserType {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateCast {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreatePolicy {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogAlterPolicy {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogCreateRule {
            txn_id,
            descriptor_json,
        }
        | WalRecord::CatalogSetComment {
            txn_id,
            descriptor_json,
        } => {
            w.write_u64(txn_id.get());
            w.write_bytes(descriptor_json)?;
        }
        WalRecord::CatalogDropPolicy {
            txn_id,
            policy_name,
            table_name,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(policy_name)?;
            w.write_str(table_name)?;
        }
        WalRecord::CatalogDropRule {
            txn_id,
            rule_name,
            table_name,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(rule_name)?;
            w.write_str(table_name)?;
        }
        WalRecord::CatalogDropComment {
            txn_id,
            object_type,
            object_identity,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(object_type)?;
            w.write_str(object_identity)?;
        }
        WalRecord::CatalogDropDomain {
            txn_id,
            domain_name,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(domain_name)?;
        }
        WalRecord::CatalogDropUserType { txn_id, type_name } => {
            w.write_u64(txn_id.get());
            w.write_str(type_name)?;
        }
        WalRecord::CatalogDropCast {
            txn_id,
            source_type,
            target_type,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(source_type)?;
            w.write_str(target_type)?;
        }
        WalRecord::CatalogDropSchema {
            txn_id,
            schema_id_raw,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(*schema_id_raw);
        }
        WalRecord::CatalogDropRole { txn_id, role_name } => {
            w.write_u64(txn_id.get());
            w.write_str(role_name)?;
        }
        WalRecord::CatalogDropView {
            txn_id,
            view_id_raw,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(*view_id_raw);
        }
        WalRecord::CatalogDropSequence {
            txn_id,
            sequence_id_raw,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(*sequence_id_raw);
        }
        WalRecord::CatalogDropFunction {
            txn_id,
            function_name,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(function_name)?;
        }
        WalRecord::CatalogDropTrigger {
            txn_id,
            trigger_name,
            table_name,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(trigger_name)?;
            w.write_str(table_name)?;
        }
        WalRecord::CatalogDropTable {
            txn_id,
            table_id_raw,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(*table_id_raw);
        }
        WalRecord::CatalogDropIndex {
            txn_id,
            index_id_raw,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(*index_id_raw);
        }
        WalRecord::CatalogDropTenant {
            txn_id,
            tenant_name,
        } => {
            w.write_u64(txn_id.get());
            w.write_str(tenant_name)?;
        }
        WalRecord::CatalogDropNodeLabel { txn_id, label_name }
        | WalRecord::CatalogDropEdgeLabel { txn_id, label_name } => {
            w.write_u64(txn_id.get());
            w.write_str(label_name)?;
        }
        WalRecord::CatalogSetSequenceValue {
            txn_id,
            sequence_id_raw,
            current_value,
            is_called,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(*sequence_id_raw);
            w.write_i64(*current_value);
            w.write_bool(*is_called);
        }
        _ => {} // Non-catalog records handled elsewhere.
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

fn read_txn_json(r: &mut BinaryReader<'_>) -> DbResult<(TxnId, Vec<u8>)> {
    let txn_id = TxnId::new(r.read_u64()?);
    let json = r.read_bytes()?;
    Ok((txn_id, json))
}

fn read_txn_u64(r: &mut BinaryReader<'_>) -> DbResult<(TxnId, u64)> {
    let txn_id = TxnId::new(r.read_u64()?);
    let val = r.read_u64()?;
    Ok((txn_id, val))
}

fn read_txn_str(r: &mut BinaryReader<'_>) -> DbResult<(TxnId, String)> {
    let txn_id = TxnId::new(r.read_u64()?);
    let s = r.read_str()?;
    Ok((txn_id, s))
}

pub(super) fn read_catalog_payload(tag: u8, r: &mut BinaryReader<'_>) -> DbResult<WalRecord> {
    match tag {
        13 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateSchema {
                txn_id,
                descriptor_json,
            })
        }
        14 => {
            let (txn_id, schema_id_raw) = read_txn_u64(r)?;
            Ok(WalRecord::CatalogDropSchema {
                txn_id,
                schema_id_raw,
            })
        }
        15 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateRole {
                txn_id,
                descriptor_json,
            })
        }
        16 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogAlterRole {
                txn_id,
                descriptor_json,
            })
        }
        17 => {
            let (txn_id, role_name) = read_txn_str(r)?;
            Ok(WalRecord::CatalogDropRole { txn_id, role_name })
        }
        18 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateView {
                txn_id,
                descriptor_json,
            })
        }
        19 => {
            let (txn_id, view_id_raw) = read_txn_u64(r)?;
            Ok(WalRecord::CatalogDropView {
                txn_id,
                view_id_raw,
            })
        }
        20 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateSequence {
                txn_id,
                descriptor_json,
            })
        }
        21 => {
            let (txn_id, sequence_id_raw) = read_txn_u64(r)?;
            Ok(WalRecord::CatalogDropSequence {
                txn_id,
                sequence_id_raw,
            })
        }
        22 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogAlterSequence {
                txn_id,
                descriptor_json,
            })
        }
        23 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateFunction {
                txn_id,
                descriptor_json,
            })
        }
        24 => {
            let (txn_id, function_name) = read_txn_str(r)?;
            Ok(WalRecord::CatalogDropFunction {
                txn_id,
                function_name,
            })
        }
        25 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateTrigger {
                txn_id,
                descriptor_json,
            })
        }
        26 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let trigger_name = r.read_str()?;
            let table_name = r.read_str()?;
            Ok(WalRecord::CatalogDropTrigger {
                txn_id,
                trigger_name,
                table_name,
            })
        }
        27 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogGrantPrivilege {
                txn_id,
                descriptor_json,
            })
        }
        28 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogRevokePrivilege {
                txn_id,
                descriptor_json,
            })
        }
        29 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogSetTableDescriptor {
                txn_id,
                descriptor_json,
            })
        }
        40 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogSetIndexDescriptor {
                txn_id,
                descriptor_json,
            })
        }
        41 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateTenant {
                txn_id,
                descriptor_json,
            })
        }
        42 => {
            let (txn_id, tenant_name) = read_txn_str(r)?;
            Ok(WalRecord::CatalogDropTenant {
                txn_id,
                tenant_name,
            })
        }
        43 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let sequence_id_raw = r.read_u64()?;
            let current_value = r.read_i64()?;
            let is_called = r.read_bool()?;
            Ok(WalRecord::CatalogSetSequenceValue {
                txn_id,
                sequence_id_raw,
                current_value,
                is_called,
            })
        }
        33 => {
            let (txn_id, table_id_raw) = read_txn_u64(r)?;
            Ok(WalRecord::CatalogDropTable {
                txn_id,
                table_id_raw,
            })
        }
        34 => {
            let (txn_id, index_id_raw) = read_txn_u64(r)?;
            Ok(WalRecord::CatalogDropIndex {
                txn_id,
                index_id_raw,
            })
        }
        35 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogUpdateStatistics {
                txn_id,
                descriptor_json,
            })
        }
        36 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateNodeLabel {
                txn_id,
                descriptor_json,
            })
        }
        37 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateEdgeLabel {
                txn_id,
                descriptor_json,
            })
        }
        38 => {
            let (txn_id, label_name) = read_txn_str(r)?;
            Ok(WalRecord::CatalogDropNodeLabel { txn_id, label_name })
        }
        39 => {
            let (txn_id, label_name) = read_txn_str(r)?;
            Ok(WalRecord::CatalogDropEdgeLabel { txn_id, label_name })
        }
        71 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateDomain {
                txn_id,
                descriptor_json,
            })
        }
        72 => {
            let (txn_id, domain_name) = read_txn_str(r)?;
            Ok(WalRecord::CatalogDropDomain {
                txn_id,
                domain_name,
            })
        }
        73 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogAlterDomain {
                txn_id,
                descriptor_json,
            })
        }
        74 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateUserType {
                txn_id,
                descriptor_json,
            })
        }
        75 => {
            let (txn_id, type_name) = read_txn_str(r)?;
            Ok(WalRecord::CatalogDropUserType { txn_id, type_name })
        }
        76 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogAlterUserType {
                txn_id,
                descriptor_json,
            })
        }
        77 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateCast {
                txn_id,
                descriptor_json,
            })
        }
        78 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let source_type = r.read_str()?;
            let target_type = r.read_str()?;
            Ok(WalRecord::CatalogDropCast {
                txn_id,
                source_type,
                target_type,
            })
        }
        79 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreatePolicy {
                txn_id,
                descriptor_json,
            })
        }
        80 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let policy_name = r.read_str()?;
            let table_name = r.read_str()?;
            Ok(WalRecord::CatalogDropPolicy {
                txn_id,
                policy_name,
                table_name,
            })
        }
        81 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogAlterPolicy {
                txn_id,
                descriptor_json,
            })
        }
        82 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogCreateRule {
                txn_id,
                descriptor_json,
            })
        }
        83 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let rule_name = r.read_str()?;
            let table_name = r.read_str()?;
            Ok(WalRecord::CatalogDropRule {
                txn_id,
                rule_name,
                table_name,
            })
        }
        84 => {
            let (txn_id, descriptor_json) = read_txn_json(r)?;
            Ok(WalRecord::CatalogSetComment {
                txn_id,
                descriptor_json,
            })
        }
        85 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let object_type = r.read_str()?;
            let object_identity = r.read_str()?;
            Ok(WalRecord::CatalogDropComment {
                txn_id,
                object_type,
                object_identity,
            })
        }
        _ => Err(DbError::internal(format!(
            "WAL: unknown catalog record tag {tag}"
        ))),
    }
}
