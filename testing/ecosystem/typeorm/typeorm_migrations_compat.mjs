import { DataSource } from "typeorm";
import { spawnSync } from "node:child_process";

const checks = [
  "migration_run_v1",
  "migration_run_v2",
  "migration_catalog",
  "migration_undo_v2",
];

const baseDatabaseUrl = process.env.DATABASE_URL;
if (!baseDatabaseUrl) {
  throw new Error("DATABASE_URL must be set");
}

const adminUrl = new URL(baseDatabaseUrl);
adminUrl.pathname = "/default";
const harnessDbName = `typeorm_migrations_${Date.now()}`;
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

class InitTypeormHarnessMigration1746837600000 {
  name = "InitTypeormHarnessMigration1746837600000";

  async up(queryRunner) {
    await queryRunner.query(`
      CREATE TABLE "xtask_typeorm_migration_users" (
        "id" SERIAL NOT NULL,
        "email" character varying(190) NOT NULL,
        "name" character varying(120) NOT NULL,
        CONSTRAINT "PK_xtask_typeorm_migration_users" PRIMARY KEY ("id"),
        CONSTRAINT "UQ_xtask_typeorm_migration_users_email" UNIQUE ("email")
      )
    `);
    await queryRunner.query(`
      INSERT INTO "xtask_typeorm_migration_users" ("email", "name")
      VALUES ('alice@example.com', 'Alice')
    `);
  }

  async down(queryRunner) {
    await queryRunner.query(`DROP TABLE "xtask_typeorm_migration_users"`);
  }
}

class EvolveTypeormHarnessMigration1746837601000 {
  name = "EvolveTypeormHarnessMigration1746837601000";

  async up(queryRunner) {
    await queryRunner.query(
      `ALTER TABLE "xtask_typeorm_migration_users" ADD COLUMN "summary" character varying(60)`,
    );
    await queryRunner.query(
      `ALTER TABLE "xtask_typeorm_migration_users" DROP COLUMN "name"`,
    );
    await queryRunner.query(
      `ALTER TABLE "xtask_typeorm_migration_users" ADD COLUMN "name" character varying(140) NOT NULL`,
    );
    await queryRunner.query(
      `UPDATE "xtask_typeorm_migration_users" SET "name" = 'Alice v2', "summary" = 'seeded' WHERE "email" = 'alice@example.com'`,
    );
  }

  async down(queryRunner) {
    await queryRunner.query(
      `ALTER TABLE "xtask_typeorm_migration_users" DROP COLUMN "name"`,
    );
    await queryRunner.query(
      `ALTER TABLE "xtask_typeorm_migration_users" ADD COLUMN "name" character varying(120) NOT NULL`,
    );
    await queryRunner.query(
      `UPDATE "xtask_typeorm_migration_users" SET "name" = 'Alice' WHERE "email" = 'alice@example.com'`,
    );
    await queryRunner.query(
      `ALTER TABLE "xtask_typeorm_migration_users" DROP COLUMN "summary"`,
    );
  }
}

function buildDataSource(migrations) {
  return new DataSource({
    type: "postgres",
    url: harnessUrl.toString(),
    synchronize: false,
    migrations,
    logging: process.env.TYPEORM_LOGGING === "1" ? ["query", "schema", "error"] : false,
  });
}

recreateDatabase(harnessDbName);

let migrationRows;
let v2Columns;
let revertedColumns;
let revertedNameLength;

const ds1 = buildDataSource([InitTypeormHarnessMigration1746837600000]);
let ds2;

try {
  await ds1.initialize();
  await ds1.runMigrations({ transaction: "all" });

  ds2 = buildDataSource([
    InitTypeormHarnessMigration1746837600000,
    EvolveTypeormHarnessMigration1746837601000,
  ]);
  await ds2.initialize();
  await ds2.runMigrations({ transaction: "all" });

  migrationRows = await ds2.query(`
    SELECT "timestamp", "name"
    FROM "migrations"
    ORDER BY "timestamp"
  `);
  v2Columns = await ds2.query(`
    SELECT column_name, character_maximum_length
    FROM information_schema.columns
    WHERE table_name = 'xtask_typeorm_migration_users'
    ORDER BY ordinal_position
  `);

  await ds2.undoLastMigration({ transaction: "all" });
  revertedColumns = await ds2.query(`
    SELECT column_name, character_maximum_length
    FROM information_schema.columns
    WHERE table_name = 'xtask_typeorm_migration_users'
    ORDER BY ordinal_position
  `);
  revertedNameLength = revertedColumns.find((row) => row.column_name === "name")
    ?.character_maximum_length;

  if (migrationRows.length !== 2) {
    throw new Error(`expected 2 migration rows, got ${migrationRows.length}`);
  }
  const nameColumn = v2Columns.find((row) => row.column_name === "name");
  const summaryColumn = v2Columns.find((row) => row.column_name === "summary");
  if (Number(nameColumn?.character_maximum_length) !== 140) {
    throw new Error(`expected migrated name length 140, got ${nameColumn?.character_maximum_length}`);
  }
  if (Number(summaryColumn?.character_maximum_length) !== 60) {
    throw new Error(`expected migrated summary length 60, got ${summaryColumn?.character_maximum_length}`);
  }
  if (revertedColumns.some((row) => row.column_name === "summary")) {
    throw new Error("expected summary column to disappear after undo");
  }
  if (Number(revertedNameLength) !== 120) {
    throw new Error(`expected reverted name length 120, got ${revertedNameLength}`);
  }

  process.stdout.write(
    JSON.stringify(
      {
        status: "pass",
        checks,
        observed: {
          migrationRows,
          v2Columns,
          revertedColumns,
        },
      },
      null,
      2,
    ) + "\n",
  );
} finally {
  await ds1.destroy().catch(() => {});
  await ds2?.destroy().catch(() => {});
  dropDatabaseIfExists(harnessDbName);
}
