import { createRequire } from "node:module";

const require = createRequire(
  `${process.env.AIONDB_NODE_RESOLVE_BASE}/package.json`,
);
const { MikroORM, EntitySchema, Collection } = require("@mikro-orm/core");
const { PostgreSqlDriver } = require("@mikro-orm/postgresql");
const { Client } = require("pg");

if (!process.env.DATABASE_URL) {
  throw new Error("DATABASE_URL must be set");
}

const tableUsers = "xtask_mikroorm_users";
const tablePosts = "xtask_mikroorm_posts";

class User {
  posts = new Collection(this);
}

class Post {}

const UserSchema = new EntitySchema({
  class: User,
  tableName: tableUsers,
  properties: {
    id: { type: "number", primary: true, autoincrement: true },
    email: { type: "string", length: 160, unique: true },
    name: { type: "string", length: 80 },
    posts: {
      kind: "1:m",
      entity: () => Post,
      mappedBy: "author",
    },
  },
});

const PostSchema = new EntitySchema({
  class: Post,
  tableName: tablePosts,
  properties: {
    id: { type: "number", primary: true, autoincrement: true },
    title: { type: "string", length: 140 },
    published: { type: "boolean", default: false },
    author: {
      kind: "m:1",
      entity: () => User,
      fieldName: "user_id",
      nullable: false,
    },
  },
  indexes: [{ name: `${tablePosts}_user_title_uniq`, properties: ["author", "title"], options: { unique: true } }],
});

const result = {
  checks: [
    "connect",
    "schema_create",
    "persist_and_flush",
    "populate",
    "transaction_rollback",
    "schema_diff_noop",
    "sqlstate",
  ],
};

let orm;
let em;
let databaseUrl = process.env.DATABASE_URL;
let ephemeralDatabaseName;

try {
  const bootstrapUrl = new URL(process.env.DATABASE_URL);
  const bootstrapClient = new Client({
    connectionString: process.env.DATABASE_URL,
    ssl: false,
  });
  await bootstrapClient.connect();
  ephemeralDatabaseName = `mikroorm_smoke_${Date.now().toString(36)}`;
  await bootstrapClient.query(`CREATE DATABASE ${ephemeralDatabaseName}`);
  await bootstrapClient.end();

  bootstrapUrl.pathname = `/${ephemeralDatabaseName}`;
  databaseUrl = bootstrapUrl.toString();

  orm = await MikroORM.init({
    entities: [UserSchema, PostSchema],
    clientUrl: databaseUrl,
    driver: PostgreSqlDriver,
    debug: false,
  });
  em = orm.em.fork();
  result.connected = true;

  await em.getConnection().execute(`DROP TABLE IF EXISTS "${tablePosts}" CASCADE`);
  await em.getConnection().execute(`DROP TABLE IF EXISTS "${tableUsers}" CASCADE`);
  await orm.schema.createSchema();
  result.schemaCreate = "ok";

  const user = new User();
  user.email = "alice@example.com";
  user.name = "Alice";

  const hello = new Post();
  hello.title = "hello";
  hello.published = false;
  hello.author = user;

  const world = new Post();
  world.title = "world";
  world.published = true;
  world.author = user;

  user.posts.add(hello, world);
  await em.persistAndFlush(user);
  result.persisted = { email: user.email, postCount: user.posts.length };

  em.clear();
  const loaded = await em.findOne(
    User,
    { email: "alice@example.com" },
    { populate: ["posts"], orderBy: { posts: { id: "asc" } } },
  );
  result.loaded = {
    email: loaded?.email ?? null,
    titles: loaded?.posts?.getItems().map((post) => post.title) ?? [],
  };

  await em.transactional(async (trxEm) => {
    const bob = new User();
    bob.email = "bob@example.com";
    bob.name = "Bob";
    await trxEm.persistAndFlush(bob);
    throw new Error("rollback_probe");
  }).catch((error) => {
    if (error.message !== "rollback_probe") {
      throw error;
    }
  });
  result.countAfterRollback = await em.count(User, {});

  const diff = await orm.schema.getUpdateSchemaSQL();
  result.schemaDiffNoop = diff.trim();

  try {
    await em.getConnection().execute("SELECT * FROM xtask_mikroorm_missing");
  } catch (error) {
    result.sqlstate = error.code ?? error?.nativeError?.code ?? null;
  }

  if (result.persisted.email !== "alice@example.com") {
    throw new Error(`unexpected persisted email: ${result.persisted.email}`);
  }
  if (result.persisted.postCount !== 2) {
    throw new Error(`expected 2 persisted posts, got ${result.persisted.postCount}`);
  }
  if (JSON.stringify(result.loaded.titles) !== JSON.stringify(["hello", "world"])) {
    throw new Error(`unexpected loaded titles: ${JSON.stringify(result.loaded.titles)}`);
  }
  if (result.countAfterRollback !== 1) {
    throw new Error(`rollback should leave 1 user, got ${result.countAfterRollback}`);
  }
  if (result.schemaDiffNoop !== "") {
    throw new Error(`expected empty schema diff, got: ${result.schemaDiffNoop}`);
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
  await orm?.close(true).catch(() => {});
  if (ephemeralDatabaseName) {
    const bootstrapClient = new Client({
      connectionString: process.env.DATABASE_URL,
      ssl: false,
    });
    await bootstrapClient.connect().catch(() => {});
    await bootstrapClient
      .query(`DROP DATABASE ${ephemeralDatabaseName}`)
      .catch(() => {});
    await bootstrapClient.end().catch(() => {});
  }
}

process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
