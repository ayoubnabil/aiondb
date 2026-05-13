import { createRequire } from "node:module";

const require = createRequire(
  `${process.env.AIONDB_NODE_RESOLVE_BASE}/package.json`,
);
const knexLib = require("knex");

if (!process.env.DATABASE_URL) {
  throw new Error("DATABASE_URL must be set");
}

const knex = knexLib({
  client: "pg",
  connection: {
    connectionString: process.env.DATABASE_URL,
    ssl: false,
  },
  pool: { min: 0, max: 4 },
});

const users = "xtask_knex_users";
const posts = "xtask_knex_posts";

const result = {
  checks: [
    "connect",
    "schema_builder_create",
    "crud",
    "column_info",
    "transaction_rollback",
    "schema_builder_alter",
    "sqlstate",
  ],
};

try {
  await knex.raw("SELECT 1");
  result.connected = true;

  await knex.schema.dropTableIfExists(posts);
  await knex.schema.dropTableIfExists(users);

  await knex.schema.createTable(users, (table) => {
    table.increments("id").primary();
    table.string("email", 160).notNullable().unique();
    table.string("name", 80).notNullable();
  });

  await knex.schema.createTable(posts, (table) => {
    table.increments("id").primary();
    table
      .integer("user_id")
      .notNullable()
      .references("id")
      .inTable(users);
    table.string("title", 140).notNullable();
    table.boolean("published").notNullable().defaultTo(false);
    table.unique(["user_id", "title"], {
      indexName: `${posts}_user_title_uniq`,
    });
  });
  result.schemaBuilderCreate = "ok";

  const [aliceId] = await knex(users)
    .insert({ email: "alice@example.com", name: "Alice" })
    .returning("id");
  const resolvedAliceId = typeof aliceId === "object" ? aliceId.id : aliceId;
  await knex(posts).insert([
    { user_id: resolvedAliceId, title: "hello", published: false },
    { user_id: resolvedAliceId, title: "world", published: true },
  ]);

  const joined = await knex(users)
    .leftJoin(posts, `${users}.id`, `${posts}.user_id`)
    .where(`${users}.id`, resolvedAliceId)
    .orderBy(`${posts}.id`)
    .select(`${users}.email`, `${posts}.title`);
  result.joined = {
    email: joined[0]?.email ?? null,
    titles: joined.map((row) => row.title).filter(Boolean),
  };

  result.columnInfo = await knex(posts).columnInfo();

  await knex.transaction(async (trx) => {
    await trx(users).insert({ email: "bob@example.com", name: "Bob" });
    throw new Error("rollback_probe");
  }).catch((error) => {
    if (error.message !== "rollback_probe") {
      throw error;
    }
  });
  result.countAfterRollback = Number((await knex(users).count("* as count"))[0].count);

  await knex.schema.alterTable(posts, (table) => {
    table.string("summary", 60);
  });
  result.schemaBuilderAlter = "ok";

  result.columnInfoAfterAlter = await knex(posts).columnInfo();

  try {
    await knex.raw("SELECT * FROM xtask_knex_missing");
  } catch (error) {
    result.sqlstate = error.code ?? error?.nativeError?.code ?? null;
  }

  if (result.joined.email !== "alice@example.com") {
    throw new Error(`unexpected joined email: ${result.joined.email}`);
  }
  if (JSON.stringify(result.joined.titles) !== JSON.stringify(["hello", "world"])) {
    throw new Error(`unexpected joined titles: ${JSON.stringify(result.joined.titles)}`);
  }
  if (!("summary" in result.columnInfoAfterAlter)) {
    throw new Error("summary column missing after alter");
  }
  if (result.countAfterRollback !== 1) {
    throw new Error(`rollback should leave 1 user, got ${result.countAfterRollback}`);
  }
  if (result.sqlstate !== "42P01") {
    throw new Error(`expected SQLSTATE 42P01, got ${result.sqlstate}`);
  }

  result.status = "pass";
} catch (error) {
  result.status = "fail";
  result.error = String(error?.message ?? error);
  result.detail = error?.detail ?? error?.nativeError?.detail ?? null;
  result.hint = error?.hint ?? error?.nativeError?.hint ?? null;
  result.sqlstate = result.sqlstate ?? error?.code ?? error?.nativeError?.code ?? null;
} finally {
  await knex.destroy().catch(() => {});
}

process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
