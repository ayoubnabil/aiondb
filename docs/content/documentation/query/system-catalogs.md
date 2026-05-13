---
title: System Catalogs
order: 62
---

# System Catalogs

AionDB implements PostgreSQL-facing catalog and information schema views for compatibility with clients, drivers, and introspection queries.

## information_schema

The v0.1 surface covers the standard schema-introspection tables most drivers and ORMs depend on. Every name below is a virtual relation served by the planner:

| Group | Tables |
| --- | --- |
| Schema overview | `schemata`, `tables`, `views`, `columns`, `sequences`, `triggers`, `domains`, `routines`, `parameters` |
| Constraints | `table_constraints`, `key_column_usage`, `referential_constraints`, `constraint_column_usage` |
| Privileges | `table_privileges`, `role_table_grants`, `usage_privileges`, `role_usage_grants`, `applicable_roles`, `enabled_roles` |
| Locale | `character_sets`, `collations` |
| Foreign data | `foreign_data_wrappers`, `foreign_data_wrapper_options`, `foreign_servers`, `foreign_server_options`, `user_mappings`, `user_mapping_options`, `foreign_tables`, `foreign_table_options` |

Example:

```sql
SELECT table_schema, table_name
FROM information_schema.tables
ORDER BY table_schema, table_name;
```

Column introspection:

```sql
SELECT table_name, column_name, data_type
FROM information_schema.columns
WHERE table_schema = 'public'
ORDER BY table_name, ordinal_position;
```

`information_schema` is the safer first target for generic tools because it is standardized and usually less tied to PostgreSQL internals than `pg_catalog`.

## pg_catalog

AionDB includes virtual `pg_catalog` tables used by PostgreSQL drivers and ORMs. The current planner-served set is large enough to absorb common driver introspection paths without falling through to the general binder:

| Group | Tables |
| --- | --- |
| Schema graph | `pg_namespace`, `pg_class`, `pg_attribute`, `pg_attrdef`, `pg_type`, `pg_range`, `pg_enum`, `pg_proc`, `pg_aggregate`, `pg_operator`, `pg_opclass`, `pg_opfamily`, `pg_amop`, `pg_amproc`, `pg_am`, `pg_cast`, `pg_conversion`, `pg_collation`, `pg_language`, `pg_tablespace`, `pg_database` |
| Constraints & indexes | `pg_index`, `pg_indexes`, `pg_constraint`, `pg_inherits`, `pg_partitioned_table` |
| Views, sequences, matviews | `pg_views`, `pg_tables`, `pg_sequence`, `pg_sequences`, `pg_matviews` |
| Roles & ACLs | `pg_authid`, `pg_roles`, `pg_user`, `pg_shadow`, `pg_auth_members`, `pg_init_privs`, `pg_default_acl`, `pg_policy` |
| Statistics | `pg_statistic`, `pg_statistic_ext`, `pg_statistic_ext_data`, `pg_stats`, `pg_stats_ext`, `pg_stats_ext_exprs` |
| Runtime activity | `pg_stat_activity`, `pg_stat_database`, `pg_stat_bgwriter`, `pg_stat_archiver`, `pg_stat_io`, `pg_stat_slru`, `pg_stat_wal`, `pg_stat_wal_receiver`, `pg_locks`, `pg_prepared_statements`, `pg_prepared_xacts`, `pg_stat_statements`, `pg_cursors`, `pg_backend_memory_contexts`, `pg_shmem_allocations` |
| Per-relation stats | `pg_stat_all_tables`, `pg_stat_user_tables`, `pg_statio_all_tables`, `pg_statio_user_tables`, `pg_stat_user_indexes`, `pg_statio_user_indexes`, `pg_stat_user_functions` |
| Replication | `pg_replication_slots`, `pg_replication_origin`, `pg_stat_replication`, `pg_publication`, `pg_publication_namespace`, `pg_publication_rel`, `pg_subscription` |
| Settings & config | `pg_settings`, `pg_config`, `pg_file_settings`, `pg_hba_file_rules`, `pg_ident_file_mappings`, `pg_db_role_setting` |
| Extensions | `pg_extension`, `pg_available_extensions`, `pg_available_extension_versions`, `pg_event_trigger`, `pg_trigger`, `pg_rewrite`, `pg_rules` |
| Foreign data | `pg_foreign_server`, `pg_foreign_table`, `pg_foreign_data_wrapper`, `pg_user_mapping`, `pg_user_mappings` |
| Comments & labels | `pg_description`, `pg_shdescription`, `pg_seclabel`, `pg_depend`, `pg_shdepend`, `pg_compat_object_attrs`, `pg_compat_trigger_state` |
| Timezones | `pg_timezone_abbrevs`, `pg_timezone_names` |
| Full-text search | `pg_ts_config`, `pg_ts_dict`, `pg_ts_parser`, `pg_ts_template` |
| Large objects | `pg_largeobject`, `pg_largeobject_metadata` |

Example:

```sql
SELECT typname, oid
FROM pg_catalog.pg_type
ORDER BY oid
LIMIT 10;
```

`pg_catalog` compatibility is harder because many tools depend on PostgreSQL-specific OIDs, type names, relation metadata, function rows, and constraint details. AionDB implements compatibility where needed, but v0.1 should not be treated as a full PostgreSQL catalog clone.

## ORM introspection

ORMs often inspect:

- table names;
- column names and types;
- primary keys;
- indexes;
- constraints;
- sequences;
- server version;
- supported functions.

When an ORM fails before running application SQL, capture the catalog query it executed. That query belongs in the compatibility suite.

## Example schema inspection

```sql
CREATE TABLE catalog_demo (
    id INT PRIMARY KEY,
    body TEXT
);

SELECT column_name, data_type
FROM information_schema.columns
WHERE table_name = 'catalog_demo'
ORDER BY ordinal_position;
```

This is a useful smoke test because many drivers do the same kind of lookup automatically.

## Compatibility posture

These catalogs exist for compatibility, but they are not a full PostgreSQL catalog implementation. Some client introspection queries are supported directly; others may fall back to the general planner or fail if they rely on unsupported PostgreSQL details.

When adding ORM support, capture the exact catalog query generated by the tool.

## Reporting catalog gaps

Include:

- the client or ORM name and version;
- the catalog query;
- PostgreSQL output if used as reference;
- AionDB output or SQLSTATE;
- whether the query is required for connection, migration, or runtime behavior.
