import { EntitySchema, DataSource } from "typeorm";
import { spawnSync } from "node:child_process";

const checks = [
  "synchronize_v1",
  "crud",
  "relation_load",
  "transaction_rollback",
  "catalog_introspection",
  "synchronize_v2",
];

const baseDatabaseUrl = process.env.DATABASE_URL;
if (!baseDatabaseUrl) {
  throw new Error("DATABASE_URL must be set");
}

const adminUrl = new URL(baseDatabaseUrl);
adminUrl.pathname = "/default";
const harnessDbName = `typeorm_harness_${Date.now()}`;
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
    name: "TypeormHarnessUser",
    tableName: "xtask_typeorm_users",
    columns: {
      id: { type: Number, primary: true, generated: true },
      email: { type: "varchar", length: 190, unique: true },
      name: { type: "varchar", length: version === 1 ? 120 : 140 },
    },
    relations: {
      posts: {
        type: "one-to-many",
        target: "TypeormHarnessPost",
        inverseSide: "author",
      },
    },
  });

  const postColumns = {
    id: { type: Number, primary: true, generated: true },
    slug: { type: "varchar", length: 190, unique: true },
    title: { type: "varchar", length: 190 },
    published: { type: Boolean, default: false },
  };
  if (version >= 2) {
    postColumns.summary = { type: "varchar", length: 60, nullable: true };
  }

  const Post = new EntitySchema({
    name: "TypeormHarnessPost",
    tableName: "xtask_typeorm_posts",
    columns: postColumns,
    relations: {
      author: {
        type: "many-to-one",
        target: "TypeormHarnessUser",
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
        name: "idx_xtask_typeorm_posts_user_id",
        columns: ["author"],
      },
      {
        name: "xtask_typeorm_posts_user_title_uniq",
        columns: ["author", "title"],
        unique: true,
      },
    ],
  });

  return [User, Post];
}

function buildDataSource(version) {
  return new DataSource({
    type: "postgres",
    url: harnessUrl.toString(),
    entities: buildEntities(version),
    synchronize: true,
    logging: process.env.TYPEORM_LOGGING === "1" ? ["query", "schema", "error"] : false,
  });
}

recreateDatabase(harnessDbName);

let createdUserEmail;
let includedPostSlugs;
let rollbackCounts;
let columns;
let compositeUniqueSeen;
let nameLength;
let summaryLength;

const ds1 = buildDataSource(1);
let ds2;

try {
  await ds1.initialize();
  const userRepo = ds1.getRepository("TypeormHarnessUser");
  const postRepo = ds1.getRepository("TypeormHarnessPost");

  const alice = userRepo.create({
    email: "alice@example.com",
    name: "Alice",
  });
  await userRepo.save(alice);

  const firstPost = postRepo.create({
    slug: "hello-world",
    title: "Hello",
    published: true,
    author: alice,
  });
  const secondPost = postRepo.create({
    slug: "second-post",
    title: "Second",
    published: false,
    author: alice,
  });
  await postRepo.save([firstPost, secondPost]);
  createdUserEmail = alice.email;

  const loaded = await userRepo.findOne({
    where: { email: "alice@example.com" },
    relations: { posts: true },
    order: { posts: { id: "ASC" } },
  });
  includedPostSlugs = loaded.posts.map((post) => post.slug);

  try {
    await ds1.transaction(async (manager) => {
      const bob = manager.create("TypeormHarnessUser", {
        email: "bob@example.com",
        name: "Bob",
      });
      await manager.save("TypeormHarnessUser", bob);
      const atomic = manager.create("TypeormHarnessPost", {
        slug: "atomic-post",
        title: "Atomic",
        author: bob,
      });
      await manager.save("TypeormHarnessPost", atomic);
      throw new Error("rollback probe");
    });
  } catch (error) {
    if (error.message !== "rollback probe") {
      throw error;
    }
  }

  rollbackCounts = [
    await userRepo.count({ where: { email: "bob@example.com" } }),
    await postRepo.count({ where: { slug: "atomic-post" } }),
  ];

  const infoRows = await ds1.query(`
    SELECT column_name
    FROM information_schema.columns
    WHERE table_name = 'xtask_typeorm_posts'
    ORDER BY column_name
  `);
  columns = infoRows.map((row) => row.column_name);

  const constraintRows = await ds1.query(`
    SELECT conname
    FROM pg_catalog.pg_constraint
    WHERE conrelid = 'xtask_typeorm_posts'::regclass
    ORDER BY conname
  `);
  const indexRows = await ds1.query(`
    SELECT c.relname
    FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_class c ON c.oid = i.indexrelid
    WHERE i.indrelid = 'xtask_typeorm_posts'::regclass
      AND i.indisunique
    ORDER BY c.relname
  `);
  compositeUniqueSeen =
    constraintRows.some(
      (row) => row.conname === "xtask_typeorm_posts_user_title_uniq",
    ) ||
    indexRows.some(
      (row) => row.relname === "xtask_typeorm_posts_user_title_uniq",
    );

  await ds1.destroy();

  ds2 = buildDataSource(2);
  await ds2.initialize();
  const v2Columns = await ds2.query(`
    SELECT column_name, character_maximum_length
    FROM information_schema.columns
    WHERE table_name IN ('xtask_typeorm_users', 'xtask_typeorm_posts')
      AND column_name IN ('name', 'summary')
    ORDER BY table_name, column_name
  `);
  nameLength = v2Columns.find((row) => row.column_name === "name")?.character_maximum_length;
  summaryLength = v2Columns.find((row) => row.column_name === "summary")?.character_maximum_length;

  if (createdUserEmail !== "alice@example.com") {
    throw new Error(`expected alice@example.com, got ${createdUserEmail}`);
  }
  if (JSON.stringify(includedPostSlugs) !== JSON.stringify(["hello-world", "second-post"])) {
    throw new Error(`unexpected included posts: ${JSON.stringify(includedPostSlugs)}`);
  }
  if (JSON.stringify(rollbackCounts) !== JSON.stringify([0, 0])) {
    throw new Error(`rollback should leave [0,0], got ${JSON.stringify(rollbackCounts)}`);
  }
  if (
    JSON.stringify(columns) !==
    JSON.stringify(["id", "published", "slug", "title", "user_id"])
  ) {
    throw new Error(`unexpected columns: ${JSON.stringify(columns)}`);
  }
  if (!compositeUniqueSeen) {
    throw new Error("expected composite unique definition to be visible in PostgreSQL catalogs");
  }
  if (Number(nameLength) !== 140) {
    throw new Error(`expected evolved name length 140, got ${nameLength}`);
  }
  if (Number(summaryLength) !== 60) {
    throw new Error(`expected evolved summary length 60, got ${summaryLength}`);
  }

  process.stdout.write(
    JSON.stringify(
      {
        status: "pass",
        checks,
        observed: {
          createdUserEmail,
          includedPostSlugs,
          rollbackCounts,
          columns,
          compositeUniqueSeen,
          nameLength,
          summaryLength,
        },
      },
      null,
      2,
    ),
  );
} finally {
  if (ds1.isInitialized) {
    await ds1.destroy();
  }
  if (ds2?.isInitialized) {
    await ds2.destroy();
  }
  dropDatabaseIfExists(harnessDbName);
}
