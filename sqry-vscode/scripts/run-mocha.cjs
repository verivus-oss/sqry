#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");

const mochaPath = path.resolve(__dirname, "..", "node_modules", "mocha", "bin", "mocha.js");
const nodeArgs = supportsNoExperimentalStripTypes()
  ? ["--no-experimental-strip-types"]
  : [];

const result = spawnSync(
  process.execPath,
  [...nodeArgs, mochaPath, ...process.argv.slice(2)],
  {
    env: process.env,
    stdio: "inherit",
  },
);

if (result.error) {
  throw result.error;
}

process.exit(result.status ?? 1);

function supportsNoExperimentalStripTypes() {
  const result = spawnSync(
    process.execPath,
    ["--no-experimental-strip-types", "--version"],
    { stdio: "ignore" },
  );

  return result.status === 0;
}
