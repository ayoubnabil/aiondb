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

const tableUsers = "xtask_sequelize_users";
const tablePosts = "xtask_sequelize_posts";

const User = sequelize.define(
  "User",
  {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true,
    },
    email: {
      type: DataTypes.STRING(160),
      allowNull: false,
      unique: true,
    },
    name: {
      type: DataTypes.STRING(80),
      allowNull: false,
    },
  },
  {
    tableName: tableUsers,
    timestamps: false,
  },
);

const Post = sequelize.define(
  "Post",
  {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true,
    },
    userId: {
      type: DataTypes.INTEGER,
      allowNull: false,
      field: "user_id",
    },
    title: {
      type: DataTypes.STRING(140),
      allowNull: false,
    },
    published: {
      type: DataTypes.BOOLEAN,
      allowNull: false,
      defaultValue: false,
    },
  },
  {
    tableName: tablePosts,
    timestamps: false,
    indexes: [
      {
        unique: true,
        fields: ["user_id", "title"],
        name: `${tablePosts}_user_title_uniq`,
      },
    ],
  },
);

User.hasMany(Post, { foreignKey: "user_id", as: "posts" });
Post.belongsTo(User, { foreignKey: "user_id", as: "author" });

const result = {
  checks: [
    "connect",
    "sync",
    "crud",
    "relation_include",
    "describe_table",
    "transaction_rollback",
    "sqlstate",
  ],
};
let stage = "connect";

try {
  await sequelize.authenticate();
  result.connected = true;

  stage = "drop";
  await sequelize.drop();
  stage = "sync";
  await sequelize.sync({ force: true });
  result.sync = "ok";

  stage = "crud";
  const alice = await User.create({ email: "alice@example.com", name: "Alice" });
  await Post.bulkCreate([
    { userId: alice.id, title: "hello" },
    { userId: alice.id, title: "world", published: true },
  ]);

  stage = "relation_include";
  const loaded = await User.findOne({
    where: { id: alice.id },
    include: [{ model: Post, as: "posts", required: false }],
    order: [[{ model: Post, as: "posts" }, "id", "ASC"]],
  });
  result.loaded = {
    email: loaded?.email ?? null,
    postTitles: loaded?.posts?.map((post) => post.title) ?? [],
  };

  stage = "describe_table";
  const description = await sequelize.getQueryInterface().describeTable(tablePosts);
  result.columns = Object.keys(description).sort();
  result.columnTypes = Object.fromEntries(
    Object.entries(description).map(([name, meta]) => [name, String(meta.type)]),
  );

  stage = "transaction_rollback";
  await sequelize.transaction(async (transaction) => {
    await User.create(
      { email: "bob@example.com", name: "Bob" },
      { transaction },
    );
    throw new Error("rollback_probe");
  }).catch((error) => {
    if (error.message !== "rollback_probe") {
      throw error;
    }
  });

  result.countAfterRollback = await User.count();

  stage = "sqlstate";
  try {
    await sequelize.query("SELECT * FROM xtask_sequelize_missing");
  } catch (error) {
    result.sqlstate = error?.parent?.code ?? error?.original?.code ?? null;
  }

  if (result.loaded.email !== "alice@example.com") {
    throw new Error(`unexpected loaded email: ${result.loaded.email}`);
  }
  if (JSON.stringify(result.loaded.postTitles) !== JSON.stringify(["hello", "world"])) {
    throw new Error(`unexpected relation titles: ${JSON.stringify(result.loaded.postTitles)}`);
  }
  if (JSON.stringify(result.columns) !== JSON.stringify(["id", "published", "title", "user_id"])) {
    throw new Error(`unexpected columns: ${JSON.stringify(result.columns)}`);
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
  result.stage = stage;
  result.error = String(error?.message ?? error);
  result.detail = error?.detail ?? error?.parent?.detail ?? null;
  result.hint = error?.hint ?? error?.parent?.hint ?? null;
  result.sqlstate = result.sqlstate ?? error?.parent?.code ?? error?.original?.code ?? null;
} finally {
  await sequelize.close().catch(() => {});
}

process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
