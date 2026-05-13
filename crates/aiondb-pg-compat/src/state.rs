//! Versioned compat state.
//!
//! Groups persistable state structures for compat commands:
//! - `CompatMiscObjects` - miscellaneous objects (operators, aggregates, procedures, rules, casts) indexed by name,
//! - `DomainDefs` - normalized DOMAIN definitions,
//! - `CastDefs` - stored explicit casts,
//! - `RuleDefs` - CREATE RULE rules with their INSTEAD actions.
//!
//! Each struct carries a `schema_version` to prepare for format migrations
//! if the state is ever persisted to disk.
//!
//! The implementations are deliberately simple (HashMap/BTreeMap). The
//! goal is **testability** and consistent compat SQL tags, not runtime
//! performance.

use std::collections::BTreeMap;

pub const COMPAT_STATE_SCHEMA_VERSION: u32 = 1;

/// Stable key for a compat object: command family + canonical name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompatObjectKey {
    pub family: CompatObjectFamily,
    pub name: String,
}

impl CompatObjectKey {
    pub fn new(family: CompatObjectFamily, name: impl Into<String>) -> Self {
        Self {
            family,
            name: name.into(),
        }
    }
}

/// Stored object family, derived from compat SQL tags but specific to the
/// persistence layer (Type/Domain/Cast separation that would otherwise
/// share a single logical family).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompatObjectFamily {
    Operator,
    Aggregate,
    Procedure,
    Routine,
    Rule,
    Policy,
    Publication,
    Subscription,
    Server,
    ForeignTable,
    UserMapping,
    Collation,
    Conversion,
    Transform,
    AccessMethod,
    Tablespace,
    Statistics,
    MaterializedView,
    EventTrigger,
    Misc,
}

impl CompatObjectFamily {
    pub fn for_tag(tag: &str) -> Self {
        match tag {
            "CREATE OPERATOR" | "DROP OPERATOR" => Self::Operator,
            "CREATE AGGREGATE" | "DROP AGGREGATE" => Self::Aggregate,
            "CREATE PROCEDURE" | "DROP PROCEDURE" => Self::Procedure,
            "DROP ROUTINE" => Self::Routine,
            "CREATE RULE" | "ALTER RULE" | "DROP RULE" => Self::Rule,
            "CREATE POLICY" | "ALTER POLICY" | "DROP POLICY" => Self::Policy,
            "CREATE PUBLICATION" | "DROP PUBLICATION" => Self::Publication,
            "CREATE SUBSCRIPTION" | "DROP SUBSCRIPTION" => Self::Subscription,
            "CREATE SERVER" | "DROP SERVER" => Self::Server,
            "CREATE FOREIGN TABLE" | "DROP FOREIGN TABLE" => Self::ForeignTable,
            "CREATE USER MAPPING" | "ALTER USER MAPPING" | "DROP USER MAPPING" => Self::UserMapping,
            "CREATE COLLATION" | "DROP COLLATION" => Self::Collation,
            "CREATE CONVERSION" | "DROP CONVERSION" => Self::Conversion,
            "CREATE TRANSFORM" | "DROP TRANSFORM" => Self::Transform,
            "CREATE ACCESS METHOD" | "DROP ACCESS METHOD" => Self::AccessMethod,
            "CREATE TABLESPACE" | "DROP TABLESPACE" => Self::Tablespace,
            "CREATE STATISTICS" | "DROP STATISTICS" => Self::Statistics,
            "CREATE MATERIALIZED VIEW" | "DROP MATERIALIZED VIEW" => Self::MaterializedView,
            "CREATE EVENT TRIGGER" | "DROP EVENT TRIGGER" => Self::EventTrigger,
            _ => Self::Misc,
        }
    }
}

/// Attributes of a stored compat object. Labels are optional to stay close
/// to the `pg_catalog.pg_compat_object_attrs` format.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatObjectAttrs {
    pub owner: Option<String>,
    pub schema: Option<String>,
    pub state: Option<String>,
    pub options: Option<String>,
    pub tablespace: Option<String>,
    pub version: Option<String>,
}

/// Registry of compat objects (operators, aggregates, procedures, rules, policies, ...).
#[derive(Debug, Clone)]
pub struct CompatMiscObjects {
    pub schema_version: u32,
    objects: BTreeMap<CompatObjectKey, CompatObjectAttrs>,
}

impl Default for CompatMiscObjects {
    fn default() -> Self {
        Self {
            schema_version: COMPAT_STATE_SCHEMA_VERSION,
            objects: BTreeMap::new(),
        }
    }
}

impl CompatMiscObjects {
    pub fn insert(
        &mut self,
        key: CompatObjectKey,
        attrs: CompatObjectAttrs,
    ) -> Option<CompatObjectAttrs> {
        self.objects.insert(key, attrs)
    }

    pub fn remove(&mut self, key: &CompatObjectKey) -> Option<CompatObjectAttrs> {
        self.objects.remove(key)
    }

    pub fn get(&self, key: &CompatObjectKey) -> Option<&CompatObjectAttrs> {
        self.objects.get(key)
    }

    pub fn contains(&self, key: &CompatObjectKey) -> bool {
        self.objects.contains_key(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&CompatObjectKey, &CompatObjectAttrs)> {
        self.objects.iter()
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn by_family(
        &self,
        family: CompatObjectFamily,
    ) -> impl Iterator<Item = (&CompatObjectKey, &CompatObjectAttrs)> {
        self.objects
            .iter()
            .filter(move |(key, _)| key.family == family)
    }
}

/// Normalized definition of a PG-compat DOMAIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainDef {
    pub name: String,
    pub base_type: String,
    pub default_expr: Option<String>,
    pub not_null: bool,
    pub check_constraints: Vec<DomainCheckConstraint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainCheckConstraint {
    pub name: Option<String>,
    pub expression_sql: String,
}

#[derive(Debug, Clone)]
pub struct DomainDefs {
    pub schema_version: u32,
    domains: BTreeMap<String, DomainDef>,
}

impl Default for DomainDefs {
    fn default() -> Self {
        Self {
            schema_version: COMPAT_STATE_SCHEMA_VERSION,
            domains: BTreeMap::new(),
        }
    }
}

impl DomainDefs {
    pub fn insert(&mut self, def: DomainDef) -> Option<DomainDef> {
        self.domains.insert(def.name.clone(), def)
    }

    pub fn remove(&mut self, name: &str) -> Option<DomainDef> {
        self.domains.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&DomainDef> {
        self.domains.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &DomainDef)> {
        self.domains.iter()
    }

    pub fn len(&self) -> usize {
        self.domains.len()
    }

    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }
}

/// Normalized definition of a CAST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastDef {
    pub source_type: String,
    pub target_type: String,
    pub context: CastContextKind,
    pub method: CastMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastContextKind {
    Explicit,
    Assignment,
    Implicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CastMethod {
    Binary,
    InOut,
    Function(String),
}

#[derive(Debug, Clone)]
pub struct CastDefs {
    pub schema_version: u32,
    casts: BTreeMap<(String, String), CastDef>,
}

impl Default for CastDefs {
    fn default() -> Self {
        Self {
            schema_version: COMPAT_STATE_SCHEMA_VERSION,
            casts: BTreeMap::new(),
        }
    }
}

impl CastDefs {
    pub fn insert(&mut self, def: CastDef) -> Option<CastDef> {
        self.casts
            .insert((def.source_type.clone(), def.target_type.clone()), def)
    }

    pub fn remove(&mut self, source_type: &str, target_type: &str) -> Option<CastDef> {
        self.casts
            .remove(&(source_type.to_owned(), target_type.to_owned()))
    }

    pub fn get(&self, source_type: &str, target_type: &str) -> Option<&CastDef> {
        self.casts
            .get(&(source_type.to_owned(), target_type.to_owned()))
    }

    pub fn contains(&self, source_type: &str, target_type: &str) -> bool {
        self.casts
            .contains_key(&(source_type.to_owned(), target_type.to_owned()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &CastDef> {
        self.casts.values()
    }

    pub fn len(&self) -> usize {
        self.casts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.casts.is_empty()
    }
}

/// Definition of a RULE (CREATE RULE ... AS ON ... DO INSTEAD ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDef {
    pub name: String,
    pub relation: String,
    pub event: RuleEvent,
    pub instead: bool,
    pub action_sql: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleEvent {
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone)]
pub struct RuleDefs {
    pub schema_version: u32,
    rules: BTreeMap<String, RuleDef>,
}

impl Default for RuleDefs {
    fn default() -> Self {
        Self {
            schema_version: COMPAT_STATE_SCHEMA_VERSION,
            rules: BTreeMap::new(),
        }
    }
}

impl RuleDefs {
    pub fn insert(&mut self, def: RuleDef) -> Option<RuleDef> {
        self.rules.insert(def.name.clone(), def)
    }

    pub fn remove(&mut self, name: &str) -> Option<RuleDef> {
        self.rules.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&RuleDef> {
        self.rules.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.rules.contains_key(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &RuleDef> {
        self.rules.values()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misc_objects_roundtrip() {
        let mut reg = CompatMiscObjects::default();
        assert_eq!(reg.schema_version, COMPAT_STATE_SCHEMA_VERSION);
        assert!(reg.is_empty());

        let key = CompatObjectKey::new(CompatObjectFamily::Operator, "+=");
        let mut attrs = CompatObjectAttrs::default();
        attrs.owner = Some("aion".into());

        assert!(reg.insert(key.clone(), attrs.clone()).is_none());
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get(&key).unwrap().owner.as_deref(), Some("aion"));

        let removed = reg.remove(&key).unwrap();
        assert_eq!(removed.owner.as_deref(), Some("aion"));
        assert!(reg.is_empty());
    }

    #[test]
    fn by_family_filters_correctly() {
        let mut reg = CompatMiscObjects::default();
        reg.insert(
            CompatObjectKey::new(CompatObjectFamily::Operator, "+"),
            CompatObjectAttrs::default(),
        );
        reg.insert(
            CompatObjectKey::new(CompatObjectFamily::Aggregate, "my_agg"),
            CompatObjectAttrs::default(),
        );
        let operators: Vec<_> = reg.by_family(CompatObjectFamily::Operator).collect();
        assert_eq!(operators.len(), 1);
        assert_eq!(operators[0].0.name, "+");
    }

    #[test]
    fn object_family_is_derived_from_compat_tag() {
        assert_eq!(
            CompatObjectFamily::for_tag("CREATE OPERATOR"),
            CompatObjectFamily::Operator
        );
        assert_eq!(
            CompatObjectFamily::for_tag("ALTER USER MAPPING"),
            CompatObjectFamily::UserMapping
        );
        assert_eq!(
            CompatObjectFamily::for_tag("CREATE OR REPLACE"),
            CompatObjectFamily::Misc
        );
    }

    #[test]
    fn domain_defs_roundtrip() {
        let mut reg = DomainDefs::default();
        let def = DomainDef {
            name: "positive_int".into(),
            base_type: "integer".into(),
            default_expr: None,
            not_null: true,
            check_constraints: vec![DomainCheckConstraint {
                name: Some("chk_positive".into()),
                expression_sql: "value > 0".into(),
            }],
        };
        assert!(reg.insert(def.clone()).is_none());
        assert_eq!(reg.get("positive_int"), Some(&def));
        assert_eq!(reg.remove("positive_int"), Some(def));
    }

    #[test]
    fn cast_defs_roundtrip() {
        let mut reg = CastDefs::default();
        let def = CastDef {
            source_type: "text".into(),
            target_type: "int4".into(),
            context: CastContextKind::Explicit,
            method: CastMethod::Function("pg_catalog.text_to_int".into()),
        };
        assert!(reg.insert(def.clone()).is_none());
        assert!(reg.contains("text", "int4"));
        assert_eq!(reg.get("text", "int4"), Some(&def));
        let removed = reg.remove("text", "int4").unwrap();
        assert_eq!(removed.context, CastContextKind::Explicit);
    }

    #[test]
    fn rule_defs_roundtrip() {
        let mut reg = RuleDefs::default();
        let def = RuleDef {
            name: "my_rule".into(),
            relation: "public.logs".into(),
            event: RuleEvent::Insert,
            instead: true,
            action_sql: "NOTHING".into(),
        };
        assert!(reg.insert(def.clone()).is_none());
        assert!(reg.contains("my_rule"));
        // Duplicate insert returns the previous def.
        let prev = reg.insert(def.clone()).unwrap();
        assert_eq!(prev.action_sql, "NOTHING");
        assert_eq!(reg.len(), 1);
    }
}
