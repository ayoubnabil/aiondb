-- pgweb: host="*" title="AionDB Cypher MATCH"
MATCH (d:doc)-[:related_doc]->(next:doc)
RETURN d.id AS source_id, next.id AS target_id, d.title AS source_label, next.title AS target_label
LIMIT 50;
