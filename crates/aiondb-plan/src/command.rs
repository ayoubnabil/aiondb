//! `CommandPlan` - typed plans for utility commands
//! (non-query) that currently execute inline in the engine.
//!
//! Goal (see ADR-0001 and the binder validation policy): centralize the
//! SET / SHOW / LISTEN / NOTIFY / CHECKPOINT / DISCARD / ALTER SYSTEM /
//! COMMENT / DO / LOAD commands into a typed plan, built by the binder and
//! consumed by the executor. No direct dispatch on `Statement::*` in the
//! runtime engine.
//!
//! This module is a **scaffold** - the variants are defined but not yet
//! consumed by the engine. Migration will happen command by command, each
//! with its ADR where applicable.

use aiondb_core::DataType;

/// Typed plan for a non-query utility command.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CommandPlan {
    /// `SET [SESSION|LOCAL] <var> = <value>`.
    SetVariable {
        name: String,
        value: CommandValue,
        local: bool,
    },
    /// `SHOW <var>`.
    ShowVariable {
        name: String,
    },
    /// `RESET <var>` / `RESET ALL`.
    ResetVariable {
        target: ResetTarget,
    },
    /// `SET CONSTRAINTS {ALL | <names>} {DEFERRED | IMMEDIATE}`.
    SetConstraints {
        target: ConstraintTarget,
        deferred: bool,
    },
    /// `LISTEN <channel>` / `UNLISTEN <channel|*>`.
    Listen {
        channel: String,
    },
    Unlisten {
        channel: UnlistenTarget,
    },
    /// `NOTIFY <channel> [, <payload>]`.
    Notify {
        channel: String,
        payload: Option<String>,
    },
    /// `CHECKPOINT`.
    Checkpoint,
    /// `DISCARD {ALL | PLANS | SEQUENCES | TEMP | TEMPORARY}`.
    Discard {
        target: DiscardTarget,
    },
    /// `ALTER SYSTEM SET <var> = <value>` / `RESET <var>`.
    AlterSystem {
        name: String,
        value: Option<CommandValue>,
    },
    /// `LOAD '<library>'`.
    Load {
        library: String,
    },
    /// `DO [LANGUAGE <lang>] $$ ... $$`.
    Do {
        language: String,
        body: String,
    },
    /// `COMMENT ON <object_type> <name> IS '<comment>'`.
    Comment {
        object_description: String,
        comment: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PgObjectKind {
    Type,
    Domain,
    Cast,
    Rule,
    Policy,
    Publication,
    Subscription,
    Server,
    UserMapping,
    ForeignTable,
    ForeignDataWrapper,
    Collation,
    Statistics,
    Tablespace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PgObjectAction {
    Create,
    Alter,
    Drop,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CommandValue {
    Literal {
        data_type: DataType,
        rendered: String,
    },
    DefaultKeyword,
    /// For `SET x = y, z` list values (GUCs that accept comma-separated lists).
    List(Vec<CommandValue>),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ResetTarget {
    All,
    Name(String),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConstraintTarget {
    All,
    Names(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UnlistenTarget {
    Channel(String),
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiscardTarget {
    All,
    Plans,
    Sequences,
    Temp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_roundtrip() {
        let plan = CommandPlan::SetVariable {
            name: "timezone".into(),
            value: CommandValue::Literal {
                data_type: DataType::Text,
                rendered: "'UTC'".into(),
            },
            local: false,
        };
        let json = serde_json::to_string(&plan).expect("serialize");
        let back: CommandPlan = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(plan, back);
    }

    #[test]
    fn discard_variants_distinct() {
        assert_ne!(
            CommandPlan::Discard {
                target: DiscardTarget::All,
            },
            CommandPlan::Discard {
                target: DiscardTarget::Plans,
            }
        );
    }
}
