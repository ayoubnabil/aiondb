#![allow(clippy::pedantic)]

use aiondb_core::DbResult;
use aiondb_parser::Statement;

use super::super::Engine;
use crate::{prepared::StatementResult, session::SessionHandle};

impl Engine {
    pub(in super::super) fn emit_ddl_audit(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        result: &DbResult<StatementResult>,
    ) {
        use crate::auth_audit::DdlAuditEvent;
        let obj_name = |n: &aiondb_parser::ObjectName| n.parts.join(".");
        let (operation, object_type, object_name): (&str, &str, String) = match statement {
            Statement::CreateTable(s) => ("CREATE", "TABLE", obj_name(&s.name)),
            Statement::CreateTableAs(s) => ("CREATE", "TABLE", obj_name(&s.name)),
            Statement::TruncateTable(s) => ("TRUNCATE", "TABLE", obj_name(&s.name)),
            Statement::DropTable(s) => ("DROP", "TABLE", obj_name(&s.name)),
            Statement::AlterTable(s) => ("ALTER", "TABLE", obj_name(&s.table)),
            Statement::CreateIndex(s) => ("CREATE", "INDEX", obj_name(&s.name)),
            Statement::DropIndex(s) => ("DROP", "INDEX", obj_name(&s.name)),
            Statement::CreateSequence(s) => ("CREATE", "SEQUENCE", obj_name(&s.name)),
            Statement::DropSequence(s) => ("DROP", "SEQUENCE", obj_name(&s.name)),
            Statement::CreateView(s) => ("CREATE", "VIEW", obj_name(&s.name)),
            Statement::DropView(s) => ("DROP", "VIEW", obj_name(&s.name)),
            Statement::CreateRole(s) => ("CREATE", "ROLE", s.name.clone()),
            Statement::DropRole(s) => ("DROP", "ROLE", s.name.clone()),
            Statement::AlterRole(s) => ("ALTER", "ROLE", s.name.clone()),
            Statement::Grant(s) => {
                let privs = s
                    .privileges
                    .iter()
                    .map(format_privilege)
                    .collect::<Vec<_>>()
                    .join(", ");
                let target = format_grant_target(&s.target);
                (
                    "GRANT",
                    "PRIVILEGE",
                    format!("{privs} ON {target} TO {}", s.role_name),
                )
            }
            Statement::Revoke(s) => {
                let privs = s
                    .privileges
                    .iter()
                    .map(format_privilege)
                    .collect::<Vec<_>>()
                    .join(", ");
                let target = format_grant_target(&s.target);
                (
                    "REVOKE",
                    "PRIVILEGE",
                    format!("{privs} ON {target} FROM {}", s.role_name),
                )
            }
            _ => return,
        };
        let role = self
            .with_session(session, |r| Ok(r.info.identity.user.clone()))
            .unwrap_or_else(|_| "<unknown>".to_owned());
        let (success, error_message) = match result {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e.report().message.clone())),
        };
        self.auth_audit_sink.record_ddl(DdlAuditEvent {
            role,
            operation: operation.to_owned(),
            object_type: object_type.to_owned(),
            object_name,
            success,
            error_message,
        });
    }
}

fn format_privilege(p: &aiondb_parser::Privilege) -> &'static str {
    use aiondb_parser::Privilege;
    match p {
        Privilege::Select => "SELECT",
        Privilege::Insert => "INSERT",
        Privilege::Update => "UPDATE",
        Privilege::Delete => "DELETE",
        Privilege::All => "ALL",
        Privilege::Create => "CREATE",
        Privilege::Usage => "USAGE",
        Privilege::Execute => "EXECUTE",
        Privilege::Trigger => "TRIGGER",
        Privilege::References => "REFERENCES",
        Privilege::Connect => "CONNECT",
        Privilege::Temporary => "TEMPORARY",
        Privilege::Truncate => "TRUNCATE",
    }
}

fn format_grant_target(t: &aiondb_parser::GrantTarget) -> String {
    use aiondb_parser::GrantTarget;
    match t {
        GrantTarget::Table(name) => format!("TABLE {}", name.parts.join(".")),
        GrantTarget::Function(target) => {
            let signature = target
                .arg_types
                .as_ref()
                .map_or_else(String::new, |arg_types| {
                    let args = arg_types
                        .iter()
                        .map(aiondb_core::DataType::pg_type_name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("({args})")
                });
            format!("FUNCTION {}{}", target.name.parts.join("."), signature)
        }
        GrantTarget::Schema(name) => format!("SCHEMA {name}"),
        GrantTarget::Database(name) => format!("DATABASE {name}"),
        GrantTarget::Role(name) => format!("ROLE {name}"),
    }
}

pub(crate) fn is_graph_ddl_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateNodeLabel(_)
            | Statement::CreateEdgeLabel(_)
            | Statement::DropNodeLabel(_)
            | Statement::DropEdgeLabel(_)
    )
}
