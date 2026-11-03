// The host-identity probes packages run, as one expression evaluating to { probe: string }.
// Paths are normalized: `<execPath>` is process.execPath, `<script>` the running script.
(() => {
  const p = globalThis.process;
  const norm = (v) => (v === undefined ? "undefined" : v === p.execPath ? "<execPath>" : String(v));
  // context-mode build/db-base.js `hasModernSqlite()` (picks node:sqlite over better-sqlite3).
  const hasModernSqlite = () => {
    if (typeof globalThis.Bun !== "undefined" && globalThis.Bun !== null) return true;
    const [major, minor] = (p.versions.node ?? "0.0.0").split(".").map(Number);
    return major > 22 || (major === 22 && minor >= 5);
  };
  // pi-subagents src/runs/shared/pi-spawn.js `resolveBunPiExecutable()`.
  const compiledPi = !!p.versions.bun && !!p.argv[1] && /^(?:\/\$bunfs\/|B:[\\/]~BUN[\\/])/.test(p.argv[1]);
  return {
    "process.versions.bun": norm(p.versions.bun),
    "typeof globalThis.Bun": typeof globalThis.Bun,
    "process.versions.node (major)": p.versions.node.split(".")[0],
    "process.release.name": p.release.name,
    "process.argv[0]": norm(p.argv[0]),
    "process.argv[1]": norm(p.argv[1]),
    "process.argv.slice(2)": JSON.stringify(p.argv.slice(2)),
    "process.env.PI_SUBAGENT_PI_BINARY": norm(p.env.PI_SUBAGENT_PI_BINARY),
    "pi-subagents resolveBunPiExecutable() != undefined": String(compiledPi),
    "context-mode hasModernSqlite()": String(hasModernSqlite()),
    "synckit/scripted-workflow Bun branch (typeof versions.bun == string)": String(typeof p.versions.bun === "string"),
  };
})()
