MATCH (n:Person) RETURN n;
MATCH (a)-[*1..3]->(b) RETURN b;
CALL db.labels() YIELD label RETURN label;
