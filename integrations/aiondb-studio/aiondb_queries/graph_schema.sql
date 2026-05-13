-- pgweb: host="*" title="AionDB graph labels"
CREATE TABLE docs (id INT PRIMARY KEY, title TEXT, embedding VECTOR(2));
CREATE TABLE doc_links (source_id INT NOT NULL, target_id INT NOT NULL, relation TEXT);

CREATE NODE LABEL doc ON docs;
CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc;
