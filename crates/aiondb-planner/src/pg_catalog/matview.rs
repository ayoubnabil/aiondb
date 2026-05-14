use aiondb_catalog::{QualifiedName, ViewDescriptor};

const MATVIEW_SIDECAR_MARKER: &str = "aiondb:matview";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MatviewSidecarMetadata {
    pub relation_name: QualifiedName,
    pub relispopulated: bool,
}

pub(crate) fn parse_matview_sidecar(view: &ViewDescriptor) -> Option<MatviewSidecarMetadata> {
    let body = matview_marker_body(&view.query_sql)?;
    let mut relation_name: Option<QualifiedName> = None;
    let mut relispopulated = true;

    for token in body[MATVIEW_SIDECAR_MARKER.len()..].split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        if key.eq_ignore_ascii_case("table") || key.eq_ignore_ascii_case("name") {
            relation_name = Some(resolve_relation_name(view, value));
            continue;
        }
        if key.eq_ignore_ascii_case("populated") || key.eq_ignore_ascii_case("relispopulated") {
            relispopulated = parse_marker_bool(value).unwrap_or(relispopulated);
        }
    }

    Some(MatviewSidecarMetadata {
        relation_name: relation_name.unwrap_or_else(|| {
            QualifiedName::new(
                view.name.schema_name().map(str::to_owned),
                view.name.object_name().to_owned(),
            )
        }),
        relispopulated,
    })
}

fn matview_marker_body(query_sql: &str) -> Option<&str> {
    let sql = query_sql.trim_start();
    let marker = sql.strip_prefix("/*")?.split_once("*/")?.0.trim();
    let prefix = marker.get(..MATVIEW_SIDECAR_MARKER.len())?;
    if !prefix.eq_ignore_ascii_case(MATVIEW_SIDECAR_MARKER) {
        return None;
    }
    Some(marker)
}

fn resolve_relation_name(view: &ViewDescriptor, value: &str) -> QualifiedName {
    let parsed = QualifiedName::parse(value);
    if parsed.schema_name().is_some() {
        parsed
    } else {
        QualifiedName::new(
            view.name.schema_name().map(str::to_owned),
            parsed.object_name().to_owned(),
        )
    }
}

fn parse_marker_bool(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("t")
        || value.eq_ignore_ascii_case("yes")
        || value == "1"
    {
        return Some(true);
    }
    if value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("f")
        || value.eq_ignore_ascii_case("no")
        || value == "0"
    {
        return Some(false);
    }
    None
}

#[cfg(test)]
mod tests {
    use aiondb_catalog::{ColumnDescriptor, ViewDescriptor};
    use aiondb_core::{ColumnId, DataType, RelationId, SchemaId};

    use super::*;

    fn sidecar_view(query_sql: &str) -> ViewDescriptor {
        ViewDescriptor {
            view_id: RelationId::new(7),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "__aiondb_matview_sales"),
            query_sql: query_sql.to_owned(),
            creation_search_path_schemas: Vec::new(),
            check_option: None,
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
        }
    }

    #[test]
    fn parses_marked_sidecar_metadata() {
        let metadata = parse_matview_sidecar(&sidecar_view(
            "/* aiondb:matview table=sales_snapshot populated=false */ SELECT id FROM sales_snapshot",
        ))
        .expect("sidecar should be recognized");

        assert_eq!(
            metadata.relation_name,
            QualifiedName::qualified("public", "sales_snapshot")
        );
        assert!(!metadata.relispopulated);
    }

    #[test]
    fn ignores_unmarked_views() {
        assert!(parse_matview_sidecar(&sidecar_view("SELECT id FROM sales")).is_none());
    }
}
