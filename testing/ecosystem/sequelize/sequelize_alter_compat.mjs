import { createRequire } from "node:module";

const require = createRequire(
  `${process.env.AIONDB_NODE_RESOLVE_BASE}/package.json`,
);
const { Sequelize, DataTypes } = require("sequelize");

if (!process.env.DATABASE_URL) {
  throw new Error("DATABASE_URL must be set");
}

const sequelize = new Sequelize(process.env.DATABASE_URL, {
  dialect: "postgres",
  logging: false,
  dialectOptions: { ssl: false },
});

const table = "xtask_sequelize_alter_posts";

function defineV1() {
  return sequelize.define(
    "AlterPostV1",
    {
      id: {
        type: DataTypes.INTEGER,
        primaryKey: true,
        autoIncrement: true,
      },
      title: {
        type: DataTypes.STRING(80),
        allowNull: false,
      },
      published: {
        type: DataTypes.BOOLEAN,
        allowNull: false,
        defaultValue: false,
      },
    },
    {
      tableName: table,
      timestamps: false,
    },
  );
}

function defineV2() {
  return sequelize.define(
    "AlterPostV2",
    {
      id: {
        type: DataTypes.INTEGER,
        primaryKey: true,
        autoIncrement: true,
      },
      title: {
        type: DataTypes.STRING(140),
        allowNull: false,
      },
      summary: {
        type: DataTypes.STRING(60),
        allowNull: true,
      },
      published: {
        type: DataTypes.BOOLEAN,
        allowNull: false,
        defaultValue: false,
      },
    },
    {
      tableName: table,
      timestamps: false,
    },
  );
}

const result = {
  checks: ["sync_v1", "sync_alter_v2", "sync_alter_noop", "describe_table"],
};

try {
  await sequelize.authenticate();
  await sequelize.drop();

  const V1 = defineV1();
  await V1.sync({ force: true });
  result.syncV1 = "ok";

  const V2 = defineV2();
  await V2.sync({ alter: true });
  result.syncAlterV2 = "ok";

  const descriptionAfterAlter = await sequelize.getQueryInterface().describeTable(table);
  result.afterAlter = {
    columns: Object.keys(descriptionAfterAlter).sort(),
    types: Object.fromEntries(
      Object.entries(descriptionAfterAlter).map(([name, meta]) => [name, String(meta.type)]),
    ),
  };

  await V2.sync({ alter: true });
  result.syncAlterNoop = "ok";

  const descriptionAfterNoop = await sequelize.getQueryInterface().describeTable(table);
  result.afterNoop = {
    columns: Object.keys(descriptionAfterNoop).sort(),
    types: Object.fromEntries(
      Object.entries(descriptionAfterNoop).map(([name, meta]) => [name, String(meta.type)]),
    ),
  };

  if (
    JSON.stringify(result.afterAlter.columns) !==
    JSON.stringify(["id", "published", "summary", "title"])
  ) {
    throw new Error(`unexpected columns after alter: ${JSON.stringify(result.afterAlter.columns)}`);
  }
  if (result.afterAlter.types.title !== "CHARACTER VARYING(140)") {
    throw new Error(`unexpected title type after alter: ${result.afterAlter.types.title}`);
  }
  if (result.afterAlter.types.summary !== "CHARACTER VARYING(60)") {
    throw new Error(`unexpected summary type after alter: ${result.afterAlter.types.summary}`);
  }
  if (JSON.stringify(result.afterNoop) !== JSON.stringify(result.afterAlter)) {
    throw new Error("noop alter changed reflected schema");
  }

  result.status = "pass";
} catch (error) {
  result.status = "fail";
  result.error = String(error?.message ?? error);
  result.detail = error?.detail ?? error?.parent?.detail ?? null;
  result.hint = error?.hint ?? error?.parent?.hint ?? null;
  result.sqlstate = error?.parent?.code ?? error?.original?.code ?? null;
} finally {
  await sequelize.close().catch(() => {});
}

process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
