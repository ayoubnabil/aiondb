use std::sync::Arc;

use aiondb_embedded::{ConnectOptions, Database};
use aiondb_engine::{
    Credential, DataType, DbResult, EngineBuilder, PortalBatch, PreparedStatementDesc, QueryEngine,
    ResultColumn, SecretString, StatementResult,
};
use aiondb_parser::{parse_sql, Statement};
use aiondb_pgwire::format::value_to_text;

use crate::scenario::{
    command_completion_tag, scenario_error_from_db_error, ColumnOutcome, ExecutionBatchOutcome,
    PortalOutcome, PreparedOutcome, PreparedStatementOutcome, ScenarioOperation, ScenarioOutcome,
    ScenarioResult, SqlScenario, StatementOutcome,
};

pub fn run_embedded(scenario: &SqlScenario) -> DbResult<ScenarioResult> {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    run_embedded_with_engine(engine, scenario)
}

pub(crate) fn run_embedded_with_engine<E>(
    engine: Arc<E>,
    scenario: &SqlScenario,
) -> DbResult<ScenarioResult>
where
    E: QueryEngine + 'static,
{
    let database = Database::<E>::new(Arc::clone(&engine));
    let connection = match database.connect(ConnectOptions {
        database: scenario.database.clone(),
        credential: scenario_credential(engine.as_ref(), scenario),
        application_name: Some(format!("embedded:{}", scenario.name)),
    }) {
        Ok(connection) => connection,
        Err(error) => return Ok(ScenarioResult::Error(scenario_error_from_db_error(&error))),
    };

    match run_embedded_inner(&connection, scenario) {
        Ok(outcome) => Ok(ScenarioResult::Success(outcome)),
        Err(error) => Ok(ScenarioResult::Error(scenario_error_from_db_error(&error))),
    }
}

fn scenario_credential<E: QueryEngine>(engine: &E, scenario: &SqlScenario) -> Credential {
    match scenario.password.as_deref() {
        Some(password) => Credential::CleartextPassword {
            user: scenario.user.clone(),
            password: SecretString::new(password.to_owned()),
        },
        None if engine.requires_password() => Credential::CleartextPassword {
            user: scenario.user.clone(),
            password: SecretString::new(String::new()),
        },
        None => Credential::Anonymous {
            user: scenario.user.clone(),
        },
    }
}

fn run_embedded_inner<E>(
    connection: &aiondb_embedded::Connection<E>,
    scenario: &SqlScenario,
) -> DbResult<ScenarioOutcome>
where
    E: QueryEngine + 'static,
{
    if let Some(setup_sql) = &scenario.setup_sql {
        let _ = connection.execute(setup_sql)?;
    }

    match &scenario.operation {
        ScenarioOperation::Simple { sql } => {
            let results = connection.execute(sql)?;
            Ok(ScenarioOutcome::Simple(statement_outcomes_from_sql(
                sql, &results,
            )?))
        }
        ScenarioOperation::Prepared {
            sql,
            params,
            max_rows,
        } => {
            let prepared = connection.prepare(format!("stmt_{}", scenario.name), sql)?;
            let portal_name = format!("portal_{}", scenario.name);
            let mut batch = prepared.execute(
                portal_name.clone(),
                params.iter().map(|value| value.to_engine_value()).collect(),
                *max_rows,
            )?;
            let portal = PortalOutcome {
                result_columns: batch.columns.iter().map(column_outcome).collect(),
            };
            let mut executions = vec![execution_batch_outcome_from_portal_batch(&batch)];
            while !batch.exhausted && !batch.columns.is_empty() {
                batch = prepared.resume(&portal_name, *max_rows)?;
                executions.push(execution_batch_outcome_from_portal_batch(&batch));
            }
            let verification = scenario
                .verify_sql
                .as_ref()
                .map(|verify_sql| {
                    connection
                        .execute(verify_sql)
                        .and_then(|results| statement_outcomes_from_sql(verify_sql, &results))
                })
                .transpose()?;

            Ok(ScenarioOutcome::Prepared(PreparedOutcome {
                statement: prepared_statement_outcome(prepared.descriptor()),
                portal,
                executions,
                verification,
            }))
        }
    }
}

fn statement_outcomes_from_sql(
    sql: &str,
    results: &[StatementResult],
) -> DbResult<Vec<StatementOutcome>> {
    let statements = parse_sql(sql)?;
    let mut statement_index = 0_usize;
    let mut outcomes = Vec::new();
    for result in results {
        if matches!(result, StatementResult::Notice { .. }) {
            continue;
        }
        let statement = statements.get(statement_index);
        outcomes.push(statement_outcome_from_result(statement, result));
        statement_index = statement_index.saturating_add(1);
    }
    Ok(outcomes)
}

fn statement_outcome_from_result(
    statement: Option<&Statement>,
    result: &StatementResult,
) -> StatementOutcome {
    match result {
        StatementResult::Query { columns, rows } => StatementOutcome::Query {
            columns: columns.iter().map(|column| column.name.clone()).collect(),
            rows: rows
                .iter()
                .map(|row| row.values.iter().map(value_to_text).collect())
                .collect(),
            completion_tag: query_completion_tag(statement, rows.len()),
        },
        StatementResult::Command { tag, rows_affected } => StatementOutcome::Command {
            completion_tag: command_completion_tag(tag, *rows_affected),
        },
        StatementResult::CopyIn { .. } | StatementResult::CopyOut { .. } => {
            StatementOutcome::Command {
                completion_tag: "COPY".to_owned(),
            }
        }
        StatementResult::Notice { .. } => {
            // Notices are transport/session side-channels, not statement
            // results for parity comparisons.
            StatementOutcome::Command {
                completion_tag: String::new(),
            }
        }
    }
}

fn query_completion_tag(statement: Option<&Statement>, row_count: usize) -> String {
    match statement {
        Some(Statement::Explain { .. }) => "EXPLAIN".to_owned(),
        Some(Statement::ShowVariable(_)) => "SHOW".to_owned(),
        _ => format!("SELECT {row_count}"),
    }
}

fn execution_batch_outcome_from_portal_batch(batch: &PortalBatch) -> ExecutionBatchOutcome {
    if batch.columns.is_empty() {
        ExecutionBatchOutcome::CommandComplete {
            completion_tag: command_completion_tag(&batch.tag, batch.rows_affected),
        }
    } else {
        let rows = batch
            .rows
            .iter()
            .map(|row| row.values.iter().map(value_to_text).collect())
            .collect();
        if batch.exhausted {
            ExecutionBatchOutcome::QueryComplete {
                rows,
                completion_tag: batch.tag.clone(),
            }
        } else {
            ExecutionBatchOutcome::QuerySuspended { rows }
        }
    }
}

fn prepared_statement_outcome(desc: &PreparedStatementDesc) -> PreparedStatementOutcome {
    PreparedStatementOutcome {
        param_type_oids: desc
            .param_types
            .iter()
            .map(|data_type| data_type.pg_oid().unwrap_or(25))
            .collect(),
        result_columns: desc.result_columns.iter().map(column_outcome).collect(),
    }
}

fn column_outcome(column: &ResultColumn) -> ColumnOutcome {
    let (type_oid, type_size) = data_type_to_pg(&column.data_type);
    ColumnOutcome {
        name: column.name.clone(),
        type_oid,
        type_size,
    }
}

fn data_type_to_pg(data_type: &DataType) -> (u32, i16) {
    let oid = data_type.pg_oid().unwrap_or(25);
    let size = match data_type {
        DataType::Int | DataType::Real => 4,
        DataType::BigInt | DataType::Double => 8,
        DataType::Boolean => 1,
        _ => -1,
    };
    (oid, size)
}
