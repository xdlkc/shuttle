import { lstatSync, realpathSync, readdirSync, mkdirSync, readFileSync, writeFileSync, unlinkSync, statSync } from "node:fs";
import { resolve, relative, dirname, sep, isAbsolute } from "node:path";

export function allowedPath(root, raw) {
  if (typeof raw !== "string" || raw.includes("\0") || raw.includes("\\") || /[^\x20-\x7e\u4e00-\u9fff/]/u.test(raw) || /^[~@]/.test(raw)) return false;
  const target = resolve(root, raw || ".");
  const rel = relative(root, target);
  if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) return false;
  if (rel.split(sep).some(p => p.startsWith(".") && p !== "" || /^(providers\.json|auth\.json|config\.json|AGENTS\.md|CLAUDE\.md)$/i.test(p) || /\.(key|pem|db|sqlite3|jsonl)$/i.test(p))) return false;
  let cursor = target;
  while (cursor !== root) {
    try { if (lstatSync(cursor).isSymbolicLink()) return false; } catch (e) { if (e.code !== "ENOENT") return false; }
    const parent = dirname(cursor);
    if (parent === cursor) return false;
    cursor = parent;
  }
  return true;
}

export default function guard(pi) {
  const root = realpathSync(process.env.AGENT_RELAY_WORKSPACE || process.cwd());
  const write = process.env.AGENT_RELAY_MODE === "fix";
  pi.registerTool({
    name: "relay_file", label: "Workspace file",
    description: "Read/list ordinary source files in this task workspace; fix tasks may write complete UTF-8 files or remove individual files. No commands, credentials, hidden control files or paths outside the task workspace are accessible. Use list with path '.' to start; use write with content to implement a change.",
    parameters: { type: "object", properties: { operation: { type: "string", enum: ["list", "read", "write", "remove"] }, path: { type: "string" }, content: { type: "string" } }, required: ["operation", "path"], additionalProperties: false },
    async execute(_id, input) {
      if (!allowedPath(root, input.path)) throw new Error("Path is outside the permitted source workspace.");
      const target = resolve(root, input.path);
      let text;
      if (input.operation === "list") {
        text = readdirSync(target, { withFileTypes: true }).filter(e => allowedPath(root, resolve(target, e.name))).slice(0, 500).map(e => e.name + (e.isDirectory() ? "/" : "")).join("\n");
      } else if (input.operation === "read") {
        if (!statSync(target).isFile() || statSync(target).size > 262144) throw new Error("Not an ordinary text file or file too large.");
        text = readFileSync(target, "utf8");
        if (text.includes("\0")) throw new Error("Binary file is unavailable.");
      } else if (input.operation === "write" && write) {
        if (typeof input.content !== "string" || Buffer.byteLength(input.content) > 524288) throw new Error("Invalid or oversized content.");
        mkdirSync(dirname(target), { recursive: true });
        writeFileSync(target, input.content, { mode: 0o600 }); text = "Source file saved.";
      } else if (input.operation === "remove" && write) {
        if (!statSync(target).isFile()) throw new Error("Only individual source files may be removed.");
        unlinkSync(target); text = "Source file removed.";
      } else throw new Error("Operation is not enabled for this task.");
      return { content: [{ type: "text", text }] };
    },
  });
  pi.on("tool_call", event => {
    if (event.toolName !== "relay_file" || !allowedPath(root, event.input?.path ?? ".")) {
      return { block: true, reason: "Shuttle: this task may only read/edit ordinary source files inside its isolated workspace; command execution and private paths are disabled." };
    }
  });
}
