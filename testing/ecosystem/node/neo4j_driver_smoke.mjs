import { createRequire } from "node:module";

const require = createRequire(
  `${process.env.AIONDB_NODE_RESOLVE_BASE}/package.json`,
);
const neo4j = require("neo4j-driver");

const uri = process.env.NEO4J_URI;
const user = process.env.NEO4J_USER;
const password = process.env.NEO4J_PASSWORD;

const driver = neo4j.driver(uri, neo4j.auth.basic(user, password), {
  connectionTimeout: 5000,
});

let one;
let status;

try {
  const session = driver.session();
  try {
    const result = await session.run("RETURN 1 AS one, 'ok' AS status");
    const record = result.records[0];
    if (!record) {
      throw new Error("neo4j-driver returned no row for RETURN probe");
    }
    one = record.get("one");
    status = record.get("status");
  } finally {
    await session.close();
  }
} finally {
  await driver.close();
}

let oneValue = one;
if (neo4j.isInt?.(one)) {
  oneValue = one.toNumber();
}

if (oneValue !== 1 || status !== "ok") {
  throw new Error(
    `unexpected neo4j-driver payload: one=${JSON.stringify(one)}, normalized=${JSON.stringify(oneValue)}, status=${JSON.stringify(status)}`,
  );
}

process.stdout.write(
  JSON.stringify({
    details:
      "Neo4j JavaScript driver connected over Bolt and completed a read-only RETURN probe",
    checks: ["bolt_connect", "auth", "session", "return_probe", "neo4j_integer_normalization"],
    uri,
  }),
);
