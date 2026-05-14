import { PrismaClient } from "@prisma/client";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const checks = [
  "db_push",
  "db_pull",
  "migrate_dev",
  "migrate_deploy",
  "migrate_reset",
  "crud",
  "relation_include",
  "interactive_transaction_rollback",
  "catalog_introspection",
];

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const prismaBin = path.join(scriptDir, "node_modules/.bin/prisma");
const mainSchemaPath = path.join(scriptDir, "schema.prisma");
const baseDatabaseUrl = process.env.DATABASE_URL;

if (!baseDatabaseUrl) {
  throw new Error("DATABASE_URL must be set");
}

const adminUrl = new URL(baseDatabaseUrl);
adminUrl.pathname = "/default";
const harnessDbName = `prisma_harness_${Date.now()}`;
const harnessUrl = new URL(baseDatabaseUrl);
harnessUrl.pathname = `/${harnessDbName}`;

function recreateDatabase(databaseName) {
  const dropResult = spawnSync(
    "psql",
    [adminUrl.toString(), "-v", "ON_ERROR_STOP=1", "-c", `DROP DATABASE IF EXISTS ${databaseName}`],
    {
      cwd: scriptDir,
      env: process.env,
      encoding: "utf8",
    },
  );
  if (dropResult.status !== 0) {
    throw new Error(
      `failed to drop database ${databaseName}: ${dropResult.stderr || dropResult.stdout}`,
    );
  }

  const createResult = spawnSync(
    "psql",
    [adminUrl.toString(), "-v", "ON_ERROR_STOP=1", "-c", `CREATE DATABASE ${databaseName}`],
    {
      cwd: scriptDir,
      env: process.env,
      encoding: "utf8",
    },
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
    {
      cwd: scriptDir,
      env: process.env,
      encoding: "utf8",
    },
  );
}

recreateDatabase(harnessDbName);

process.env.DATABASE_URL = harnessUrl.toString();

const dbPush = spawnSync(
  prismaBin,
  ["db", "push", "--schema", mainSchemaPath, "--skip-generate"],
  {
    cwd: scriptDir,
    env: process.env,
    encoding: "utf8",
  },
);
if (dbPush.status !== 0) {
  throw new Error(`prisma db push failed: ${dbPush.stderr || dbPush.stdout}`);
}

const prisma = new PrismaClient({
  datasources: {
    db: {
      url: harnessUrl.toString(),
    },
  },
});

let createdUserEmail;
let includedPostSlugs;
let rollbackCounts;
let columns;
let uniqueIndexSeen;
let introspectedModels;
let migrateDevMigrations;
let migrateDbName;
let migrateDeployDbName;
let migrateDeployAppliedCount;
let migrateResetCounts;
let migrateResetColumns;

try {
  await prisma.post.deleteMany();
  await prisma.user.deleteMany();

  const alice = await prisma.user.create({
    data: {
      email: "alice@example.com",
      name: "Alice",
      posts: {
        create: [
          { slug: "hello-world", title: "Hello", published: true },
          { slug: "second-post", title: "Second", published: false },
        ],
      },
    },
  });
  createdUserEmail = alice.email;

  const loaded = await prisma.user.findUnique({
    where: { email: "alice@example.com" },
    include: {
      posts: {
        orderBy: { id: "asc" },
      },
    },
  });
  includedPostSlugs = loaded.posts.map((post) => post.slug);

  try {
    await prisma.$transaction(async (tx) => {
      const bob = await tx.user.create({
        data: {
          email: "bob@example.com",
          name: "Bob",
        },
      });
      await tx.post.create({
        data: {
          userId: bob.id,
          slug: "atomic-post",
          title: "Atomic",
        },
      });
      throw new Error("rollback probe");
    });
  } catch (error) {
    if (error.message !== "rollback probe") {
      throw error;
    }
  }

  rollbackCounts = [
    await prisma.user.count({ where: { email: "bob@example.com" } }),
    await prisma.post.count({ where: { slug: "atomic-post" } }),
  ];

  const infoRows = await prisma.$queryRawUnsafe(`
    SELECT column_name
    FROM information_schema.columns
    WHERE table_name = 'xtask_prisma_posts'
    ORDER BY column_name
  `);
  columns = infoRows.map((row) => row.column_name);

  const indexRows = await prisma.$queryRawUnsafe(`
    SELECT indexname, indexdef
    FROM pg_indexes
    WHERE tablename = 'xtask_prisma_posts'
    ORDER BY indexname
  `);
  uniqueIndexSeen = indexRows.some(
    (row) =>
      row.indexname === "xtask_prisma_posts_user_title_uniq" &&
      row.indexdef.includes("UNIQUE INDEX xtask_prisma_posts_user_title_uniq") &&
      row.indexdef.includes("(user_id, title)"),
  );

  const introspectDir = fs.mkdtempSync(path.join(os.tmpdir(), "aiondb-prisma-db-pull-"));
  const introspectSchemaPath = path.join(introspectDir, "schema.prisma");
  fs.writeFileSync(
    introspectSchemaPath,
    `datasource db {
  provider = "postgresql"
  url      = env("DATABASE_URL")
}

generator client {
  provider = "prisma-client-js"
}
`,
  );
  const dbPull = spawnSync(
    prismaBin,
    ["db", "pull", "--print", "--schema", introspectSchemaPath],
    {
      cwd: scriptDir,
      env: process.env,
      encoding: "utf8",
    },
  );
  if (dbPull.status !== 0) {
    throw new Error(`prisma db pull failed: ${dbPull.stderr || dbPull.stdout}`);
  }
  introspectedModels = [...dbPull.stdout.matchAll(/^model\s+(\S+)/gm)].map((match) => match[1]);

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
  if (!uniqueIndexSeen) {
    throw new Error("expected composite unique index to be visible in pg_indexes");
  }
  if (
    JSON.stringify(introspectedModels.sort()) !==
    JSON.stringify(["xtask_prisma_posts", "xtask_prisma_users"])
  ) {
    throw new Error(`unexpected db pull models: ${JSON.stringify(introspectedModels)}`);
  }

  const migrateDir = fs.mkdtempSync(path.join(os.tmpdir(), "aiondb-prisma-migrate-dev-"));
  const migrateSchemaPath = path.join(migrateDir, "schema.prisma");
  migrateDbName = `prisma_migrate_${Date.now()}`;
  const migrateUrl = new URL(baseDatabaseUrl);
  migrateUrl.pathname = `/${migrateDbName}`;
  recreateDatabase(migrateDbName);
  const migrateEnv = { ...process.env, DATABASE_URL: migrateUrl.toString() };
  const migrateSchemaV1 = `generator client {
  provider = "prisma-client-js"
}

datasource db {
  provider = "postgresql"
  url      = env("DATABASE_URL")
}

model xtask_prisma_migrate_users {
  id    Int                           @id @default(autoincrement())
  email String                        @unique @db.VarChar(190)
  name  String                        @db.VarChar(120)
  posts xtask_prisma_migrate_posts[]
}

model xtask_prisma_migrate_posts {
  id        Int                         @id @default(autoincrement())
  title     String                      @db.VarChar(190)
  published Boolean                     @default(false)
  user_id   Int
  author    xtask_prisma_migrate_users  @relation(fields: [user_id], references: [id], onDelete: Cascade)

  @@index([user_id])
}
`;
  fs.writeFileSync(migrateSchemaPath, migrateSchemaV1);
  const migrateInit = spawnSync(
    path.join(scriptDir, "node_modules/.bin/prisma"),
    ["migrate", "dev", "--name", "init", "--schema", migrateSchemaPath, "--skip-generate"],
    {
      cwd: migrateDir,
      env: migrateEnv,
      encoding: "utf8",
    },
  );
  if (migrateInit.status !== 0) {
    throw new Error(
      `prisma migrate dev init failed: ${migrateInit.stderr || migrateInit.stdout}`,
    );
  }
  const migrateSchemaV2 = migrateSchemaV1
    .replace("@db.VarChar(120)", "@db.VarChar(140)")
    .replace(
      'published Boolean                     @default(false)\n  user_id   Int',
      'published Boolean                     @default(false)\n  summary   String?                      @db.VarChar(60)\n  user_id   Int',
    );
  fs.writeFileSync(migrateSchemaPath, migrateSchemaV2);
  const migrateAlter = spawnSync(
    prismaBin,
    [
      "migrate",
      "dev",
      "--name",
      "alter_schema",
      "--schema",
      migrateSchemaPath,
      "--skip-generate",
    ],
    {
      cwd: migrateDir,
      env: migrateEnv,
      encoding: "utf8",
    },
  );
  if (migrateAlter.status !== 0) {
    throw new Error(
      `prisma migrate dev alter failed: ${migrateAlter.stderr || migrateAlter.stdout}`,
    );
  }
  migrateDevMigrations = fs
    .readdirSync(path.join(migrateDir, "migrations"))
    .filter((entry) => fs.statSync(path.join(migrateDir, "migrations", entry)).isDirectory())
    .sort();
  if (migrateDevMigrations.length !== 2) {
    throw new Error(
      `expected 2 prisma migrate dev migrations, got ${JSON.stringify(migrateDevMigrations)}`,
    );
  }

  migrateDeployDbName = `prisma_migrate_deploy_${Date.now()}`;
  const migrateDeployUrl = new URL(baseDatabaseUrl);
  migrateDeployUrl.pathname = `/${migrateDeployDbName}`;
  recreateDatabase(migrateDeployDbName);
  const migrateDeployEnv = { ...process.env, DATABASE_URL: migrateDeployUrl.toString() };
  const migrateDeploy = spawnSync(
    prismaBin,
    ["migrate", "deploy", "--schema", migrateSchemaPath],
    {
      cwd: migrateDir,
      env: migrateDeployEnv,
      encoding: "utf8",
    },
  );
  if (migrateDeploy.status !== 0) {
    throw new Error(
      `prisma migrate deploy failed: ${migrateDeploy.stderr || migrateDeploy.stdout}`,
    );
  }

  const appliedRows = spawnSync(
    "psql",
    [
      migrateDeployUrl.toString(),
      "-At",
      "-v",
      "ON_ERROR_STOP=1",
      "-c",
      "SELECT count(*) FROM _prisma_migrations",
    ],
    {
      cwd: migrateDir,
      env: migrateDeployEnv,
      encoding: "utf8",
    },
  );
  if (appliedRows.status !== 0) {
    throw new Error(
      `checking _prisma_migrations failed: ${appliedRows.stderr || appliedRows.stdout}`,
    );
  }
  migrateDeployAppliedCount = Number.parseInt(appliedRows.stdout.trim(), 10);
  if (migrateDeployAppliedCount !== 2) {
    throw new Error(
      `expected 2 deployed prisma migrations, got ${migrateDeployAppliedCount}`,
    );
  }

  const seedDeployData = spawnSync(
    "psql",
    [
      migrateDeployUrl.toString(),
      "-v",
      "ON_ERROR_STOP=1",
      "-c",
      "INSERT INTO xtask_prisma_migrate_users (email, name) VALUES ('deploy@example.com', 'Deploy'); \
       INSERT INTO xtask_prisma_migrate_posts (title, published, summary, user_id) VALUES ('Deploy title', true, 'summary', 1);",
    ],
    {
      cwd: migrateDir,
      env: migrateDeployEnv,
      encoding: "utf8",
    },
  );
  if (seedDeployData.status !== 0) {
    throw new Error(
      `seeding deploy database failed: ${seedDeployData.stderr || seedDeployData.stdout}`,
    );
  }

  const migrateReset = spawnSync(
    prismaBin,
    ["migrate", "reset", "--force", "--skip-generate", "--skip-seed", "--schema", migrateSchemaPath],
    {
      cwd: migrateDir,
      env: migrateDeployEnv,
      encoding: "utf8",
    },
  );
  if (migrateReset.status !== 0) {
    throw new Error(
      `prisma migrate reset failed: ${migrateReset.stderr || migrateReset.stdout}`,
    );
  }

  const resetCountRows = spawnSync(
    "psql",
    [
      migrateDeployUrl.toString(),
      "-At",
      "-v",
      "ON_ERROR_STOP=1",
      "-c",
      "SELECT (SELECT count(*) FROM xtask_prisma_migrate_users), \
              (SELECT count(*) FROM xtask_prisma_migrate_posts), \
              (SELECT count(*) FROM _prisma_migrations)",
    ],
    {
      cwd: migrateDir,
      env: migrateDeployEnv,
      encoding: "utf8",
    },
  );
  if (resetCountRows.status !== 0) {
    throw new Error(
      `checking reset row counts failed: ${resetCountRows.stderr || resetCountRows.stdout}`,
    );
  }
  migrateResetCounts = resetCountRows.stdout
    .trim()
    .split("|")
    .map((value) => Number.parseInt(value, 10));
  if (JSON.stringify(migrateResetCounts) !== JSON.stringify([0, 0, 2])) {
    throw new Error(
      `expected reset counts [0,0,2], got ${JSON.stringify(migrateResetCounts)}`,
    );
  }

  const resetColumnRows = spawnSync(
    "psql",
    [
      migrateDeployUrl.toString(),
      "-At",
      "-v",
      "ON_ERROR_STOP=1",
      "-c",
      "SELECT column_name || ':' || data_type || ':' || COALESCE(character_maximum_length::text, '') \
       FROM information_schema.columns \
       WHERE table_name = 'xtask_prisma_migrate_users' AND column_name = 'name' \
       UNION ALL \
       SELECT column_name || ':' || data_type || ':' || COALESCE(character_maximum_length::text, '') \
       FROM information_schema.columns \
       WHERE table_name = 'xtask_prisma_migrate_posts' AND column_name = 'summary' \
       ORDER BY 1",
    ],
    {
      cwd: migrateDir,
      env: migrateDeployEnv,
      encoding: "utf8",
    },
  );
  if (resetColumnRows.status !== 0) {
    throw new Error(
      `checking reset columns failed: ${resetColumnRows.stderr || resetColumnRows.stdout}`,
    );
  }
  migrateResetColumns = resetColumnRows.stdout
    .trim()
    .split("\n")
    .filter(Boolean);
  if (
    JSON.stringify(migrateResetColumns) !==
    JSON.stringify([
      "name:character varying:140",
      "summary:character varying:60",
    ])
  ) {
    throw new Error(
      `unexpected reset columns: ${JSON.stringify(migrateResetColumns)}`,
    );
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
          uniqueIndexSeen,
          introspectedModels,
          migrateDevMigrations,
          migrateDeployAppliedCount,
          migrateResetCounts,
          migrateResetColumns,
        },
      },
      null,
      2,
    ),
  );
} finally {
  await prisma.$disconnect();
  if (migrateDbName) {
    dropDatabaseIfExists(migrateDbName);
  }
  if (migrateDeployDbName) {
    dropDatabaseIfExists(migrateDeployDbName);
  }
  dropDatabaseIfExists(harnessDbName);
}
