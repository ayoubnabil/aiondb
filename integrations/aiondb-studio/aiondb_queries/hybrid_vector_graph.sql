-- pgweb: host="*" title="AionDB hybrid vector graph"
MATCH (d:doc)-[:related_doc]->(next:doc)
RETURN d.id AS source_id,
       next.id AS target_id,
       d.title AS source_label,
       next.title AS target_label,
       l2_distance(next.embedding, '[1.0,0.0]') AS dist
ORDER BY dist ASC
LIMIT 20;
