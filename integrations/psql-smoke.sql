SELECT 1;

CREATE TABLE compat_smoke (
    id INT PRIMARY KEY,
    body TEXT
);

INSERT INTO compat_smoke VALUES (1, 'ok');
SELECT body FROM compat_smoke WHERE id = 1;

BEGIN;
INSERT INTO compat_smoke VALUES (2, 'rollback');
ROLLBACK;

SELECT COUNT(*) FROM compat_smoke;
