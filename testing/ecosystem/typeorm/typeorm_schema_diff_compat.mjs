import { EntitySchema, DataSource } from "typeorm";
import { spawnSync } from "node:child_process";

const checks = [
  "seed_v2_schema",
  "schema_log_noop",
  "schema_log_changed",
];

const baseDatabaseUrl = process.env.DATABASE_URL;
if (!baseDatabaseUrl) {
  throw new Error("DATABASE_URL must be set");
}

const adminUrl = new URL(baseDatabaseUrl);
adminUrl.pathname = "/default";
const harnessDbName = `typeorm_schema_diff_${Date.now()}`;
const harnessUrl = new URL(baseDatabaseUrl);
harnessUrl.pathname = `/${harnessDbName}`;

function recreateDatabase(databaseName) {
  const dropResult = spawnSync(
    "psql",
    [adminUrl.toString(), "-v", "ON_ERROR_STOP=1", "-c", `DROP DATABASE IF EXISTS ${databaseName}`],
    { env: process.env, encoding: "utf8" },
  );
  if (dropResult.status !== 0) {
    throw new Error(
      `failed to drop database ${databaseName}: ${dropResult.stderr || dropResult.stdout}`,
    );
  }

  const createResult = spawnSync(
    "psql",
    [adminUrl.toString(), "-v", "ON_ERROR_STOP=1", "-c", `CREATE DATABASE ${databaseName}`],
    { env: process.env, encoding: "utf8" },
  );
  if (createResult.status !== 0) {
    throw new Error(
      `failed to create database ${databaseName}: ${createResult.stderr || createResult.stdout}`,
    );
  }
}

function dropDatabaseIfExists(databaseName) {
  spawnSync(
    "psql",
    [adminUrl.toString(), "-v", "ON_ERROR_STOP=1", "-c", `DROP DATABASE IF EXISTS ${databaseName}`],
    { env: process.env, encoding: "utf8" },
  );
}

function buildEntities(version) {
  const User = new EntitySchema({
    name: "TypeormDiffUser",
    tableName: "xtask_typeorm_diff_users",
    columns: {
      id: { type: Number, primary: true, generated: true },
      email: { type: "varchar", length: 190, unique: true },
      name: { type: "varchar", length: version >= 3 ? 160 : 140 },
    },
    relations: {
      posts: {
        type: "one-to-many",
        target: "TypeormDiffPost",
        inverseSide: "author",
      },
    },
  });

  const postColumns = {
    id: { type: Number, primary: true, generated: true },
    slug: { type: "varchar", length: 190, unique: true },
    title: { type: "varchar", length: 190 },
    published: { type: Boolean, default: false },
    summary: { type: "varchar", length: 60, nullable: true },
  };
  if (version >= 3) {
    postColumns.excerpt = { type: "varchar", length: 80, nullable: true };
  }

  const Post = new EntitySchema({
    name: "TypeormDiffPost",
    tableName: "xtask_typeorm_diff_posts",
    columns: postColumns,
    relations: {
      author: {
        type: "many-to-one",
        target: "TypeormDiffUser",
        joinColumn: {
          name: "user_id",
          referencedColumnName: "id",
        },
        onDelete: "CASCADE",
        nullable: false,
      },
    },
    indices: [
      {
        name: "idx_xtask_typeorm_diff_posts_user_id",
        columns: ["author"],
      },
      {
        name: "xtask_typeorm_diff_posts_user_title_uniq",
        columns: ["author", "title"],
        unique: true,
      },
    ],
  });

  return [User, Post];
}

function buildDataSource(version, synchronize) {
  return new DataSource({
    type: "postgres",
    url: harnessUrl.toString(),
    entities: buildEntities(version),
    synchronize,
    logging: process.env.TYPEORM_LOGGING === "1" ? ["query", "schema", "error"] : false,
  });
}

async function schemaLog(dataSource) {
  await dataSource.initialize();
  try {
    return await dataSource.driver.createSchemaBuilder().log();
  } finally {
    await dataSource.destroy();
  }
}

recreateDatabase(harnessDbName);

const seed = buildDataSource(2, true);
let noop;
let changed;

try {
  await seed.initialize();
  await seed.query(`
    INSERT INTO xtask_typeorm_diff_users (email, name)
    VALUES ('alice@example.com', 'Alice')
  `);
  await seed.query(`
    INSERT INTO xtask_typeorm_diff_posts (slug, title, published, user_id, summary)
    VALUES ('hello-world', 'Hello', true, 1, 'seeded')
  `);
  await seed.destroy();

  noop = buildDataSource(2, false);
  const noopSql = await schemaLog(noop);

  changed = buildDataSource(3, false);
  const changedSql = await schemaLog(changed);

  if (noopSql.upQueries.length !== 0 || noopSql.downQueries.length !== 0) {
    throw new Error(
      `expected zero noop diff queries, got up=${noopSql.upQueries.length} down=${noopSql.downQueries.length}`,
    );
  }
  if (changedSql.upQueries.length === 0) {
    throw new Error("expected changed schema diff to emit at least one up query");
  }

  const changedQueries = changedSql.upQueries.map((query) => query.query);
  const sawExcerptAdd = changedQueries.some((query) =>
    query.includes('ALTER TABLE "xtask_typeorm_diff_posts" ADD "excerpt" character varying(80)'),
  );
  const sawNameChange = changedQueries.some((query) =>
    query.includes('ALTER TABLE "xtask_typeorm_diff_users" DROP COLUMN "name"') ||
    query.includes('ALTER TABLE "xtask_typeorm_diff_users" ADD "name" character varying(160) NOT NULL'),
  );
  if (!sawExcerptAdd) {
    throw new Error(`expected schema diff to add excerpt column, got ${JSON.stringify(changedQueries)}`);
  }
  if (!sawNameChange) {
    throw new Error(`expected schema diff to change name column, got ${JSON.stringify(changedQueries)}`);
  }

  process.stdout.write(
    JSON.stringify(
      {
        status: "pass",
        checks,
        observed: {
          noopUpQueries: noopSql.upQueries.map((query) => query.query),
          changedUpQueries: changedQueries,
        },
      },
      null,
      2,
    ) + "\n",
  );
} finally {
  await seed.destroy().catch(() => {});
  await noop?.destroy().catch(() => {});
  await changed?.destroy().catch(() => {});
  dropDatabaseIfExists(harnessDbName);
}
