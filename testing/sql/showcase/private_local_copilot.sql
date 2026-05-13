-- Showcase dataset 3: private local copilot embedded in an application

CREATE TABLE projects (
    id INT NOT NULL,
    name TEXT
);

CREATE TABLE notes (
    id INT NOT NULL,
    project_id INT NOT NULL,
    title TEXT,
    embedding VECTOR(2)
);

CREATE TABLE tasks (
    id INT NOT NULL,
    project_id INT NOT NULL,
    title TEXT,
    status TEXT,
    embedding VECTOR(2)
);

CREATE TABLE note_links (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    relation TEXT
);

CREATE TABLE note_task_edges (
    source_id INT NOT NULL,
    target_id INT NOT NULL
);

CREATE TABLE intents (
    id INT NOT NULL,
    label TEXT,
    embedding VECTOR(2)
);

CREATE NODE LABEL project ON projects;
CREATE NODE LABEL note ON notes;
CREATE NODE LABEL task ON tasks;
CREATE EDGE LABEL related_note ON note_links SOURCE note TARGET note;
CREATE EDGE LABEL note_task ON note_task_edges SOURCE note TARGET task;

INSERT INTO projects VALUES
    (1, 'Desktop IDE'),
    (2, 'Secure Notes');

INSERT INTO notes VALUES
    (1, 1, 'Crash Triage Checklist', '[1.0,0.0]'),
    (2, 1, 'Extension Debugging Guide', '[0.9,0.1]'),
    (3, 1, 'Release Checklist', '[0.6,0.4]'),
    (4, 2, 'Vault Import Notes', '[0.0,1.0]');

INSERT INTO tasks VALUES
    (10, 1, 'Fix Startup Crash', 'open', '[0.98,0.02]'),
    (11, 1, 'Improve Extension Logs', 'open', '[0.85,0.15]'),
    (12, 1, 'Publish Release Notes', 'done', '[0.55,0.45]'),
    (13, 2, 'Encrypt Vault Migration', 'open', '[0.1,0.9]');

INSERT INTO note_links VALUES
    (1, 2, 'related'),
    (1, 3, 'related'),
    (4, 4, 'self');

INSERT INTO note_task_edges VALUES
    (1, 10),
    (2, 11),
    (3, 12),
    (4, 13);

INSERT INTO intents VALUES
    (1, 'debug_startup', '[0.9,0.1]'),
    (2, 'ship_release', '[0.6,0.4]');
