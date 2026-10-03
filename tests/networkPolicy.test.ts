import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import test from "node:test";

const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("the personal build cannot check or install updates through Tauri", () => {
  const pkg = JSON.parse(read("package.json"));
  assert.equal(pkg.dependencies["@tauri-apps/plugin-updater"], undefined);
  assert.doesNotMatch(read("src-tauri/Cargo.toml"), /^tauri-plugin-updater\s*=/m);
  const config = JSON.parse(read("src-tauri/tauri.conf.json"));
  assert.equal(config.plugins?.updater, undefined);
  assert.notEqual(config.bundle?.createUpdaterArtifacts, true);
  const capability = JSON.parse(read("src-tauri/capabilities/default.json"));
  assert.equal(capability.permissions.some((p: string) => p.startsWith("updater:")), false);
  assert.doesNotMatch(read("src-tauri/src/lib.rs"), /tauri_plugin_updater/);
  assert.equal(existsSync(new URL("../src/components/UpdateChecker.tsx", import.meta.url)), false);
});

test("native tray has no independent network polling loop", () => {
  assert.doesNotMatch(read("src-tauri/src/tray.rs"), /get_account_usage|refresh_account_metadata/);
});

test("the main UI no longer follows quota windows to warm up accounts", () => {
  assert.doesNotMatch(read("src/App.tsx"), /getDueAutoWarmup|runAutoWarmupForAccount|checkAutoWarmup/);
});
