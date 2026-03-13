#!/usr/bin/env node
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");
const { setTimeout: delay } = require("timers/promises");
const { runTerminalDeck, printDeck } = require("../lib/terminal-deck");
const { aethymeInvestorDeck } = require("../lib/decks/aethyme-investor-deck");

const args = process.argv.slice(2);

function usage() {
  console.log(
    [
      "Aethyme CLI",
      "",
      "Usage:",
      "  aethyme graph --repo . --output .aethyme/graph.json",
      "  aethyme deck [--print]",
      "",
      "Graph options:",
      "  --repo <path>        Repository path (default: cwd)",
      "  --output <path>      Output JSON path (default: .aethyme/graph.json)",
      "  --languages <list>   Comma-separated languages (default: python,typescript)",
      "  --repo-name <name>   Repository name (default: folder name)",
      "  --tenant-id <uuid>   Tenant ID (optional)",
      "",
      "Deck options:",
      "  --print              Print all slides without interactive controls",
      "  -h, --help           Show help",
    ].join("\n")
  );
}

function fail(message) {
  console.error(`[aethyme] ${message}`);
  process.exit(1);
}

function getArgValue(flag) {
  const prefix = `${flag}=`;
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === flag && i + 1 < args.length) {
      return args[i + 1];
    }
    if (arg.startsWith(prefix)) {
      return arg.slice(prefix.length);
    }
  }
  return null;
}

function hasFlag(flag) {
  return args.includes(flag);
}

function run(cmd, cmdArgs, options = {}) {
  const result = spawnSync(cmd, cmdArgs, {
    stdio: "inherit",
    ...options,
  });
  if (result.error) {
    fail(`${cmd} failed: ${result.error.message}`);
  }
  if (result.status !== 0) {
    fail(`${cmd} exited with code ${result.status}`);
  }
}

function checkCommand(cmd) {
  const result = spawnSync(cmd, ["--version"], { stdio: "ignore" });
  return result.status === 0;
}

function parseDatabaseUrl(url) {
  try {
    const parsed = new URL(url);
    return {
      user: decodeURIComponent(parsed.username || ""),
      password: decodeURIComponent(parsed.password || ""),
      host: parsed.hostname,
      port: parsed.port ? Number(parsed.port) : 5432,
      database: parsed.pathname.replace(/^\/+/, ""),
    };
  } catch {
    return null;
  }
}

function findCorePath(startPath) {
  let current = startPath;
  while (true) {
    const candidateMonorepo = path.join(
      current,
      "packages",
      "aethyme",
      "src",
      "indexer",
      "cli.py"
    );
    if (fs.existsSync(candidateMonorepo)) {
      return path.join(current, "packages", "aethyme");
    }

    const candidateSingle = path.join(
      current,
      "aethyme",
      "src",
      "indexer",
      "cli.py"
    );
    if (fs.existsSync(candidateSingle)) {
      return path.join(current, "aethyme");
    }

    const parent = path.dirname(current);
    if (parent === current) {
      break;
    }
    current = parent;
  }
  return null;
}

async function waitForPostgres(containerName, timeoutMs) {
  const attempts = Math.ceil(timeoutMs / 2000);
  for (let i = 0; i < attempts; i += 1) {
    const result = spawnSync(
      "docker",
      ["exec", containerName, "pg_isready", "-U", "aethyme"],
      { stdio: "ignore" }
    );
    if (result.status === 0) {
      return;
    }
    await delay(2000);
  }
  fail("PostgreSQL did not become ready in time.");
}

async function main() {
  if (args.length === 0 || hasFlag("--help") || hasFlag("-h")) {
    usage();
    return;
  }

  const command = args[0];
  if (command === "deck") {
    if (hasFlag("--print")) {
      process.stdout.write(printDeck(aethymeInvestorDeck));
      return;
    }

    runTerminalDeck(aethymeInvestorDeck);
    return;
  }

  if (command !== "graph") {
    usage();
    fail(`Unknown command: ${command}`);
  }

  const repoPath = path.resolve(getArgValue("--repo") || process.cwd());
  if (!fs.existsSync(repoPath)) {
    fail(`Repository path not found: ${repoPath}`);
  }

  const repoName = getArgValue("--repo-name") || path.basename(repoPath);
  const languagesArg = getArgValue("--languages") || "python,typescript";
  const languages = languagesArg
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean);
  const tenantId = getArgValue("--tenant-id");

  const outputArg = getArgValue("--output") || ".aethyme/graph.json";
  const outputPath = path.isAbsolute(outputArg)
    ? outputArg
    : path.join(repoPath, outputArg);

  const corePath =
    process.env.AETHYME_CORE_PATH ||
    path.resolve(__dirname, "..", "..", "aethyme");
  let resolvedCorePath = path.resolve(corePath);
  let coreMarker = path.join(resolvedCorePath, "src", "indexer", "cli.py");
  if (!fs.existsSync(coreMarker)) {
    const discovered = findCorePath(repoPath);
    if (discovered) {
      resolvedCorePath = discovered;
      coreMarker = path.join(resolvedCorePath, "src", "indexer", "cli.py");
    } else {
      fail(
        "Aethyme core not found. Set AETHYME_CORE_PATH to the core directory."
      );
    }
  }

  const composePath = path.join(resolvedCorePath, "ops", "docker-compose.yml");
  if (!fs.existsSync(composePath)) {
    fail(`Docker compose file missing: ${composePath}`);
  }

  const migrationsDir = path.join(resolvedCorePath, "migrations");
  if (!fs.existsSync(migrationsDir)) {
    fail(`Migrations directory missing: ${migrationsDir}`);
  }

  if (!checkCommand("docker")) {
    fail("Docker is required but not available on PATH.");
  }
  run("docker", ["compose", "version"]);

  const databaseUrlFromEnv =
    process.env.DATABASE_URL || process.env.AETHYME_DATABASE_URL;
  let dbPassword =
    process.env.DB_PASSWORD ||
    process.env.AETHYME_DB_PASSWORD ||
    "aethyme_dev_password";
  let databaseUrl =
    databaseUrlFromEnv ||
    `postgresql://aethyme:${dbPassword}@localhost:5433/aethyme`;

  const parsedUrl = parseDatabaseUrl(databaseUrl);
  if (!parsedUrl) {
    fail("DATABASE_URL is invalid. Expected postgresql://user:pass@host:port/db");
  }
  if (databaseUrlFromEnv && !parsedUrl.password && (process.env.DB_PASSWORD || process.env.AETHYME_DB_PASSWORD)) {
    fail("DATABASE_URL is missing a password. Include it in the URL when set.");
  }
  if (databaseUrlFromEnv && parsedUrl.password) {
    if (
      (process.env.DB_PASSWORD || process.env.AETHYME_DB_PASSWORD) &&
      parsedUrl.password !== dbPassword
    ) {
      fail("DATABASE_URL password does not match DB_PASSWORD/AETHYME_DB_PASSWORD.");
    }
    dbPassword = parsedUrl.password;
  }

  if (
    parsedUrl.host !== "localhost" &&
    parsedUrl.host !== "127.0.0.1"
  ) {
    fail(
      "DATABASE_URL must target local Docker Postgres (host localhost or 127.0.0.1)."
    );
  }
  if (parsedUrl.port !== 5433) {
    fail("DATABASE_URL must use port 5433 for Docker-managed Postgres.");
  }
  if (parsedUrl.user !== "aethyme" || parsedUrl.database !== "aethyme") {
    fail("DATABASE_URL must target aethyme@localhost:5433/aethyme.");
  }
  const scopes =
    process.env.AETHYME_DB_SCOPES ||
    process.env.REPOGRAPH_DB_SCOPES ||
    '["repo:read","repo:write","org:admin"]';

  const dockerEnv = {
    ...process.env,
    DB_PASSWORD: dbPassword,
  };

  console.log("[aethyme] Starting PostgreSQL via Docker...");
  run(
    "docker",
    ["compose", "-f", composePath, "up", "-d", "postgres"],
    { env: dockerEnv, cwd: resolvedCorePath }
  );

  console.log("[aethyme] Waiting for PostgreSQL...");
  await waitForPostgres("aethyme-postgres", 60000);

  console.log("[aethyme] Applying migrations...");
  const migrationFiles = fs
    .readdirSync(migrationsDir)
    .filter((file) => file.endsWith(".sql"))
    .sort();

  for (const file of migrationFiles) {
    run(
      "docker",
      [
        "exec",
        "-e",
        `PGPASSWORD=${dbPassword}`,
        "aethyme-postgres",
        "psql",
        "-U",
        "aethyme",
        "-d",
        "aethyme",
        "-f",
        `/docker-entrypoint-initdb.d/${file}`,
      ],
      { env: dockerEnv }
    );
  }

  const pythonCandidates = [
    process.env.AETHYME_PYTHON,
    "python3",
    "python",
  ].filter(Boolean);
  let pythonCmd = null;
  for (const candidate of pythonCandidates) {
    if (checkCommand(candidate)) {
      pythonCmd = candidate;
      break;
    }
  }
  if (!pythonCmd) {
    fail("Python 3.11+ is required but not available on PATH.");
  }

  const pythonEnv = {
    ...process.env,
    DATABASE_URL: databaseUrl,
    AETHYME_DB_SCOPES: scopes,
  };

  console.log("[aethyme] Indexing repository (SCIP-only)...");
  const indexArgs = [
    "-m",
    "src.indexer.cli",
    "index",
    repoPath,
    "--repo-name",
    repoName,
    "--scip-only",
  ];
  for (const lang of languages) {
    indexArgs.push("-l", lang);
  }
  if (tenantId) {
    indexArgs.push("--tenant-id", tenantId);
  }

  run(pythonCmd, indexArgs, { cwd: resolvedCorePath, env: pythonEnv });

  console.log("[aethyme] Exporting graph...");
  const exportArgs = [
    "-m",
    "src.indexer.export_graph",
    "--repo-name",
    repoName,
    "--output",
    outputPath,
  ];
  if (tenantId) {
    exportArgs.push("--tenant-id", tenantId);
  }
  run(pythonCmd, exportArgs, { cwd: resolvedCorePath, env: pythonEnv });

  console.log(`[aethyme] Graph written to ${outputPath}`);
}

main().catch((err) => fail(err.message));
