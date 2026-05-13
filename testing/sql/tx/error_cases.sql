-- Test: Transaction error cases
-- Validates that invalid transaction operations produce
-- the correct error codes.

-- SAVEPOINT outside a transaction should fail
-- EXPECT ERROR: SAVEPOINT sp1 => InternalError (no active transaction)

-- ROLLBACK TO SAVEPOINT outside a transaction should fail
-- EXPECT ERROR: ROLLBACK TO SAVEPOINT sp1 => InternalError

-- RELEASE SAVEPOINT outside a transaction should fail
-- EXPECT ERROR: RELEASE SAVEPOINT sp1 => InternalError

-- ROLLBACK TO nonexistent savepoint should fail
BEGIN;
-- EXPECT ERROR: ROLLBACK TO SAVEPOINT noexist => InternalError
ROLLBACK;

-- RELEASE nonexistent savepoint should fail
BEGIN;
-- EXPECT ERROR: RELEASE SAVEPOINT noexist => InternalError
ROLLBACK;

-- COMMIT with trailing garbage should fail
-- EXPECT ERROR: COMMIT garbage => SyntaxError

-- ROLLBACK with trailing garbage should fail
-- EXPECT ERROR: ROLLBACK garbage => SyntaxError

-- COMMIT outside a transaction should fail
-- EXPECT ERROR: COMMIT => Protocol error (no active transaction)

-- ROLLBACK outside a transaction should fail
-- EXPECT ERROR: ROLLBACK => Protocol error (no active transaction)

-- Cancel flag is consumed after one operation
-- (Cancel the session, then first operation fails, second succeeds)
-- After cancel:
-- EXPECT ERROR: BEGIN => QueryCanceled
-- EXPECT: BEGIN => success (flag consumed)
