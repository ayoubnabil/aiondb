import { createRequire } from "node:module";

const require = createRequire(
  `${process.env.AIONDB_NODE_RESOLVE_BASE}/package.json`,
);
const knexLib = require("knex");
const { Model } = require("objection");

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
Model.knex(knex);

const users = "xtask_objection_users";
const posts = "xtask_objection_posts";

class User extends Model {
  static get tableName() {
    return users;
  }

  static get relationMappings() {
    return {
      posts: {
        relation: Model.HasManyRelation,
        modelClass: Post,
        join: {
          from: `${users}.id`,
          to: `${posts}.user_id`,
        },
      },
    };
  }
}

class Post extends Model {
  static get tableName() {
    return posts;
  }
}

const result = {
  checks: [
    "connect",
    "schema_create",
    "insert_graph",
    "with_graph_fetched",
    "transaction_rollback",
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
    table.integer("user_id").notNullable().references("id").inTable(users);
    table.string("title", 140).notNullable();
  });
  result.schemaCreate = "ok";

  const inserted = await User.query().insertGraphAndFetch({
    email: "alice@example.com",
    name: "Alice",
    posts: [{ title: "hello" }, { title: "world" }],
  });
  result.inserted = {
    email: inserted.email,
    postCount: inserted.posts?.length ?? 0,
  };

  const loaded = await User.query()
    .findById(inserted.id)
    .withGraphFetched("posts(orderById)")
    .modifiers({
      orderById(builder) {
        builder.orderBy("id");
      },
    });
  result.loaded = {
    email: loaded?.email ?? null,
    titles: loaded?.posts?.map((post) => post.title) ?? [],
  };

  await User.transaction(async (trx) => {
    await User.query(trx).insert({ email: "bob@example.com", name: "Bob" });
    throw new Error("rollback_probe");
  }).catch((error) => {
    if (error.message !== "rollback_probe") {
      throw error;
    }
  });
  result.countAfterRollback = await User.query().resultSize();

  try {
    await knex.raw("SELECT * FROM xtask_objection_missing");
  } catch (error) {
    result.sqlstate = error.code ?? error?.nativeError?.code ?? null;
  }

  if (result.inserted.email !== "alice@example.com") {
    throw new Error(`unexpected inserted email: ${result.inserted.email}`);
  }
  if (result.inserted.postCount !== 2) {
    throw new Error(`expected 2 inserted posts, got ${result.inserted.postCount}`);
  }
  if (JSON.stringify(result.loaded.titles) !== JSON.stringify(["hello", "world"])) {
    throw new Error(`unexpected loaded titles: ${JSON.stringify(result.loaded.titles)}`);
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
