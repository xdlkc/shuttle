import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, symlinkSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import guard, { allowedPath } from "../integrations/pi-guard.mjs";

const dir = mkdtempSync(join(tmpdir(), "relay-guard-"));
try {
  mkdirSync(join(dir, "src"));
  writeFileSync(join(dir, "src/a.rs"), "fixture");
  symlinkSync(tmpdir(), join(dir, "escape"));
  for (const path of ["src/a.rs", "src/new.rs", ".", "docs/说明.md"]) assert.equal(allowedPath(dir, path), true, path);
  for (const path of ["../outside", "/etc/passwd", "~/.pi/agent/auth.json", "@/etc/passwd", ".git/config", "src/AGENTS.md", "escape/file", "auth.json", "x/private.key", "src/\u00a0../../outside"]) assert.equal(allowedPath(dir, path), false, path);
  process.env.AGENT_RELAY_WORKSPACE = dir;
  process.env.AGENT_RELAY_MODE = "fix";
  let tool, hook;
  guard({ registerTool(t) { tool = t; }, on(name, fn) { assert.equal(name, "tool_call"); hook = fn; } });
  assert.equal(tool.name, "relay_file");
  assert.equal(hook({ toolName: "bash", input: { path: ".", command: "id" } }).block, true);
  await tool.execute("fixture", { operation: "write", path: "src/new.rs", content: "fixture source" });
  assert.equal((await tool.execute("fixture", { operation: "read", path: "src/new.rs" })).content[0].text, "fixture source");
  await assert.rejects(tool.execute("fixture", { operation: "read", path: "/etc/passwd" }));
  await assert.rejects(tool.execute("fixture", { operation: "write", path: "escape/file", content: "escape" }));
  await assert.rejects(tool.execute("fixture", { operation: "bash", path: ".", content: "id" }));
  process.env.AGENT_RELAY_MODE = "analyze";
  guard({ registerTool(t) { tool = t; }, on() {} });
  await assert.rejects(tool.execute("fixture", { operation: "write", path: "src/denied.rs", content: "denied" }));
  console.log("PASS: pi guard allows source edits and rejects traversal, private paths and symlink escapes.");
} finally { rmSync(dir, { recursive: true, force: true }); }
