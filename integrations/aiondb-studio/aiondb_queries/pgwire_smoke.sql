-- pgweb: host="*" title="AionDB pgwire smoke"
SELECT 1 AS pgwire_ok, current_database() AS database_name;

SELECT table_schema, table_name
FROM information_schema.tables
ORDER BY table_schema, table_name
LIMIT 50;
