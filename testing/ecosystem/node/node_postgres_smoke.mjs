import { createRequire } from "node:module";

const require = createRequire(
  `${process.env.AIONDB_NODE_RESOLVE_BASE}/package.json`,
);
const { Client } = require("pg");

const table = "xtask_node_pg_users";
const checks = [
  "connect",
  "parameter_binding",
  "transaction_rollback",
  "information_schema",
  "sqlstate",
];

const client = new Client({
  connectionString: process.env.DATABASE_URL,
  ssl: false,
});

await client.connect();

let lookupName;
let columns;
let countAfterRollback;
let sqlstate;

try {
  await client.query(`DROP TABLE IF EXISTS ${table}`);
  await client.query(`CREATE TABLE ${table} (id INT NOT NULL, name TEXT NOT NULL)`);
  await client.query(`INSERT INTO ${table} (id, name) VALUES ($1, $2), ($3, $4)`, [
    1,
    "alice",
    2,
    "bob",
  ]);

  const lookup = await client.query(`SELECT name FROM ${table} WHERE id = $1`, [2]);
  lookupName = lookup.rows[0].name;

  const introspection = await client.query(
    `
    SELECT column_name
    FROM information_schema.columns
    WHERE table_name = 'xtask_node_pg_users'
    ORDER BY column_name
    `,
  );
  columns = introspection.rows.map((row) => row.column_name);

  await client.query("BEGIN");
  await client.query(`INSERT INTO ${table} (id, name) VALUES ($1, $2)`, [3, "carol"]);
  await client.query("ROLLBACK");

  const count = await client.query(`SELECT COUNT(*) AS count FROM ${table}`);
  countAfterRollback = Number(count.rows[0].count);

  try {
    await client.query("SELECT * FROM xtask_node_pg_missing");
  } catch (error) {
    sqlstate = error.code;
  }

  await client.query(`DROP TABLE ${table}`);
} finally {
  await client.end();
}

if (lookupName !== "bob") {
  throw new Error(`expected lookup to return "bob", got ${lookupName}`);
}
if (JSON.stringify(columns) !== JSON.stringify(["id", "name"])) {
  throw new Error(`unexpected information_schema columns: ${JSON.stringify(columns)}`);
}
if (countAfterRollback !== 2) {
  throw new Error(`rollback should leave 2 rows, got ${countAfterRollback}`);
}
if (sqlstate !== "42P01") {
  throw new Error(`expected SQLSTATE 42P01, got ${sqlstate}`);
}

process.stdout.write(
  JSON.stringify({
    details:
      "node-postgres executed bound parameters, rollback, information_schema and SQLSTATE checks",
    checks,
    observed: {
      lookupName,
      columns,
      countAfterRollback,
      sqlstate,
    },
  }),
);
