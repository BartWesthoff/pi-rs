// API surface: exports, constants, prototype and instance property shapes, constructor and
// option validation, columns(), readBigInts/returnArrays (options and setters), run() results,
// finalized statements, open/close/dispose, extensions gating, sessions and backup().
import * as sqlite from "node:sqlite";
import { DatabaseSync, StatementSync, backup, constants } from "node:sqlite";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { createRequire } from "node:module";
import { log, attempt, tempDir } from "./common.mjs";

const shape = (o) => Object.fromEntries(Reflect.ownKeys(o).map((k) => {
  const d = Object.getOwnPropertyDescriptor(o, k);
  const kind = d.get ? "get" : typeof d.value;
  return [String(k), `${kind}${d.enumerable ? "E" : ""}${d.writable ? "W" : ""}${d.configurable ? "C" : ""}`];
}).filter(([k]) => k !== "Symbol(Symbol.dispose)" && k !== "Symbol(nodejs.dispose)"));

log("exports", Object.keys(sqlite).sort());
log("require", Object.keys(createRequire(import.meta.url)("node:sqlite")).sort());
log("constants", constants);
log("db proto", shape(DatabaseSync.prototype));
log("stmt proto", shape(StatementSync.prototype));
log("dispose", typeof DatabaseSync.prototype[Symbol.dispose]);
log("lengths", [DatabaseSync.length, backup.length, DatabaseSync.name, StatementSync.name, backup.name]);
const dir = tempDir();
const db = new DatabaseSync(":memory:");
log("db own", shape(db));
log("sqlite-type", db[Symbol.for("sqlite-type")]);
log("instanceof", db instanceof DatabaseSync);
log("call without new", attempt(() => DatabaseSync(":memory:")));
log("illegal statement", attempt(() => new StatementSync()));
log("illegal invocation", attempt(() => DatabaseSync.prototype.exec.call({}, "SELECT 1")));
log("path type", attempt(() => new DatabaseSync(1)));
log("path nul", attempt(() => new DatabaseSync("a\0b")));
log("options type", attempt(() => new DatabaseSync(":memory:", 1)));
log("options undefined", attempt(() => new DatabaseSync(":memory:", undefined)));
for (const opt of ["open", "readOnly", "enableForeignKeyConstraints", "enableDoubleQuotedStringLiterals", "allowExtension", "readBigInts", "returnArrays", "allowBareNamedParameters", "allowUnknownNamedParameters"]) {
  log(`option ${opt}`, attempt(() => new DatabaseSync(":memory:", { [opt]: 1 })));
}
log("buffer path", attempt(() => { const d = new DatabaseSync(Buffer.from(join(dir, "buf.db"))); d.close(); return existsSync(join(dir, "buf.db")); }));
log("dqs default", attempt(() => db.prepare('SELECT "nope"').get()));
log("dqs enabled", attempt(() => new DatabaseSync(":memory:", { enableDoubleQuotedStringLiterals: true }).prepare('SELECT "yes" AS v').get()));
const fk = (opts) => attempt(() => { const d = new DatabaseSync(":memory:", opts); d.exec("CREATE TABLE p (id INTEGER PRIMARY KEY); CREATE TABLE c (p REFERENCES p(id))"); return [d.prepare("PRAGMA foreign_keys").get(), d.prepare("INSERT INTO c VALUES (5)").run()]; });
log("fk default", fk(undefined));
log("fk off", fk({ enableForeignKeyConstraints: false }));

db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, big INTEGER)");
const insert = db.prepare("INSERT INTO t (name, score, big) VALUES (?, ?, ?)");
log("stmt own", shape(insert));
log("run", insert.run("a", 1.5, 2n ** 60n));
log("run result proto", Object.getPrototypeOf(insert.run("b", 2, 1)) === Object.prototype);
log("run bigints", (insert.setReadBigInts(true), insert.run("c", 3, 0)));
log("run update", db.prepare("UPDATE t SET score = score + 1").run());
log("run select", db.prepare("SELECT * FROM t").run());
log("constraint", attempt(() => insert.run(null, 0, 0)));
log("columns", db.prepare("SELECT t.id, name AS label, score * 2 AS doubled, 1 AS one, big FROM t").columns());
log("columns empty", db.prepare("INSERT INTO t (name) VALUES ('x')").columns());
const read = db.prepare("SELECT id, big FROM t ORDER BY id");
log("too large number", attempt(() => read.all()));
read.setReadBigInts(true);
log("read bigints", read.all());
log("read bigints get", read.get());
read.setReturnArrays(true);
log("arrays", read.all());
log("get arrays", read.get());
log("setter type", attempt(() => read.setReturnArrays(1)));
log("setter readBigInts type", attempt(() => read.setReadBigInts(1)));
const optDb = new DatabaseSync(":memory:", { readBigInts: true, returnArrays: true });
log("options readBigInts returnArrays", [optDb.prepare("SELECT 1 AS a, 'x' AS b").get(), optDb.prepare("SELECT 1 AS a").all(), optDb.prepare("SELECT 1").run()]);
log("duplicate column names", db.prepare("SELECT 1 AS a, 2 AS a, 3 AS b").get());
log("no columns get", db.prepare("CREATE TABLE IF NOT EXISTS t (x)").get());
log("get no rows", db.prepare("SELECT 1 FROM t WHERE 0").get());
log("all no rows", db.prepare("SELECT 1 FROM t WHERE 0").all());
log("prepare error", attempt(() => db.prepare("SELEC 1")));
log("prepare type", attempt(() => db.prepare(1)));
log("location memory", db.location());
log("location type", attempt(() => db.location(1)));
log("transaction", [db.isTransaction, (db.exec("BEGIN"), db.isTransaction), (db.exec("END"), db.isTransaction)]);

log("load extension disabled", attempt(() => db.loadExtension("x")));
log("enable extension disabled", attempt(() => db.enableLoadExtension(true)));
log("enable extension type", attempt(() => db.enableLoadExtension(1)));
log("disable extension", attempt(() => db.enableLoadExtension(false)));
const ext = new DatabaseSync(":memory:", { allowExtension: true });
log("load extension missing", attempt(() => ext.loadExtension(join(dir, "missing-ext"))));
log("load extension path type", attempt(() => ext.loadExtension(1)));
log("load extension after disable", attempt(() => (ext.enableLoadExtension(false), ext.loadExtension("x"))));
ext.close();

const stmt = db.prepare("SELECT 1 AS one");
log("open twice", attempt(() => db.open()));
db.close();
log("isOpen", db.isOpen);
log("isTransaction closed", attempt(() => db.isTransaction));
log("close twice", attempt(() => db.close()));
for (const m of ["get", "all", "run", "iterate", "columns", "setReadBigInts"]) log(`finalized ${m}`, attempt(() => stmt[m](true)));
log("finalized sourceSQL", attempt(() => stmt.sourceSQL));
log("finalized expandedSQL", attempt(() => stmt.expandedSQL));
log("prepare closed", attempt(() => db.prepare("SELECT 1")));
log("location closed", attempt(() => db.location()));
db.open();
log("reopen", [db.isOpen, db.prepare("SELECT 2 AS two").get()]);
log("old statement stays finalized", attempt(() => stmt.get()));
db[Symbol.dispose]();
log("dispose closes", db.isOpen);
db[Symbol.dispose]();
log("dispose twice ok", db.isOpen);
const lazy = new DatabaseSync(":memory:", { open: false });
log("open false", [lazy.isOpen, attempt(() => lazy.exec("SELECT 1"))]);

const src = new DatabaseSync(join(dir, "src.db"));
src.exec("CREATE TABLE big (v TEXT)");
const fill = src.prepare("INSERT INTO big VALUES (?)");
src.exec("BEGIN");
for (let i = 0; i < 2000; i++) fill.run("x".repeat(200) + i);
src.exec("COMMIT");
log("session", attempt(() => typeof src.createSession));
const progress = [];
const pages = await backup(src, join(dir, "copy.db"), { rate: 50, progress: (p) => progress.push([Object.keys(p), p.totalPages > p.remainingPages]) });
log("backup pages", pages > 50);
log("backup progress", [progress.length > 1, progress[0]]);
const copy = new DatabaseSync(join(dir, "copy.db"));
log("backup copy", copy.prepare("SELECT count(*) AS n FROM big").get());
copy.close();
log("backup default rate", await backup(src, join(dir, "copy2.db")) === pages);
log("backup progress throws", await backup(src, join(dir, "copy3.db"), { rate: 1, progress: () => { throw new Error("stop"); } }).then(() => "resolved", (e) => e.message));
log("backup bad target", await backup(src, join(dir, "copy4.db"), { target: "nope" }).then(() => "resolved", (e) => ({ message: e.message, code: e.code, errcode: e.errcode })));
log("backup bad dest", await backup(src, join(dir, "no", "such", "dir.db")).then(() => "resolved", (e) => ({ message: e.message, code: e.code, errcode: e.errcode })));
for (const [name, args] of Object.entries({ dest: [src, 1], options: [src, "x", 1], rate: [src, "x", { rate: 1.5 }], source: [src, "x", { source: 1 }], target: [src, "x", { target: 1 }], progress: [src, "x", { progress: 1 }] })) {
  log(`backup arg ${name}`, attempt(() => backup(...args)));
}
src.close();
log("backup closed db", attempt(() => backup(src, "x")));
