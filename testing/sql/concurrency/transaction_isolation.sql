-- Test: Transaction isolation between sessions
-- Validates that uncommitted changes are invisible to other sessions
-- under Read Committed isolation.
--
-- NOTE: Requires two concurrent sessions (writer and reader).

-- Setup (outside any transaction)
CREATE TABLE accounts (id INT, balance INT);

-- [Writer] BEGIN;
-- [Writer] INSERT INTO accounts VALUES (1, 1000);

-- Writer can see the staged row
-- [Writer] SELECT id, balance FROM accounts;
-- EXPECT: 1 row: (1, 1000)

-- Reader should NOT see uncommitted data
-- [Reader] SELECT id, balance FROM accounts;
-- EXPECT: 0 rows

-- [Writer] COMMIT;

-- Now reader can see the committed data
-- [Reader] SELECT id, balance FROM accounts;
-- EXPECT: 1 row: (1, 1000)

-- Index creation is also isolated
-- [Writer] BEGIN;
-- [Writer] CREATE INDEX accounts_id_idx ON accounts (id);

-- Writer uses the index
-- [Writer] SELECT id FROM accounts WHERE id = 1;
-- EXPECT: uses IndexEq access path

-- Reader does NOT have the index yet
-- [Reader] SELECT id FROM accounts WHERE id = 1;
-- EXPECT: uses SeqScan access path

-- [Writer] COMMIT;

-- After commit, reader also has the index
-- [Reader] SELECT id FROM accounts WHERE id = 1;
-- EXPECT: uses IndexEq access path
