-- Showcase dataset 1: knowledge graph + semantic search for operations docs

CREATE TABLE docs (
    id INT NOT NULL,
    title TEXT,
    kind TEXT,
    embedding VECTOR(2)
);

CREATE TABLE concepts (
    id INT NOT NULL,
    name TEXT,
    kind TEXT
);

CREATE TABLE doc_links (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    relation TEXT
);

CREATE TABLE doc_mentions (
    source_id INT NOT NULL,
    target_id INT NOT NULL
);

CREATE TABLE query_vectors (
    id INT NOT NULL,
    label TEXT,
    embedding VECTOR(2)
);

CREATE NODE LABEL doc ON docs;
CREATE NODE LABEL concept ON concepts;
CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc;
CREATE EDGE LABEL mentions_concept ON doc_mentions SOURCE doc TARGET concept;

INSERT INTO docs VALUES
    (1, 'Incident Response Playbook', 'runbook', '[0.0,0.0]'),
    (2, 'Pager Escalation Guide', 'guide', '[1.0,0.0]'),
    (3, 'Postmortem Template', 'template', '[0.2,0.8]'),
    (4, 'Database Recovery Runbook', 'runbook', '[0.9,0.1]'),
    (5, 'Hiring Handbook', 'policy', '[5.0,5.0]');

INSERT INTO concepts VALUES
    (10, 'incident-response', 'topic'),
    (20, 'oncall', 'topic'),
    (30, 'database', 'topic');

INSERT INTO doc_links VALUES
    (1, 2, 'supports'),
    (1, 3, 'explains'),
    (1, 4, 'references'),
    (2, 4, 'depends_on'),
    (3, 4, 'references');

INSERT INTO doc_mentions VALUES
    (1, 10),
    (1, 20),
    (2, 10),
    (2, 20),
    (3, 10),
    (4, 10),
    (4, 30),
    (5, 30);

INSERT INTO query_vectors VALUES
    (1, 'incident_ops', '[1.0,0.0]'),
    (2, 'postmortem', '[0.0,1.0]');
