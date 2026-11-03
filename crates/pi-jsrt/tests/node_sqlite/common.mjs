// Shared helpers of the node:sqlite differential scenarios (run by /usr/bin/node for the
// goldens and by pi-jsrt-node in tests/node_sqlite.rs).
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export function tempDir() {
  const dir = mkdtempSync(join(tmpdir(), "pi-sqlite-"));
  process.on("exit", () => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// An error as a comparable record.
export function err(e) {
  // Temporary directories differ per run.
  const message = String(e.message).replace(/\/\S*?pi-sqlite-[^/\s]*/g, "<tmp>");
  const out = { name: e.name, message, code: e.code };
  if ("errcode" in e) out.errcode = e.errcode;
  if ("errstr" in e) out.errstr = e.errstr;
  return out;
}

// Run `f`, returning its value or the error it throws.
export function attempt(f) {
  try {
    return { ok: f() };
  } catch (e) {
    return { error: err(e) };
  }
}

// Plain JSON of a value: bigints, typed arrays and null-prototype objects made visible.
export function show(value) {
  return JSON.stringify(value, (_, v) => {
    if (typeof v === "bigint") return `${v}n`;
    if (ArrayBuffer.isView(v)) return { [v.constructor.name]: Array.from(new Uint8Array(v.buffer, v.byteOffset, v.byteLength)) };
    if (v !== null && typeof v === "object" && !Array.isArray(v) && Object.getPrototypeOf(v) === null) return { "[null]": { ...v } };
    return v;
  });
}

export function log(label, value) {
  console.log(`${label}: ${show(value)}`);
}
