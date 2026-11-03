// WAL journal mode, checkpoints (PASSIVE, TRUNCATE), readers during a write, location(),
// file: URL paths and readOnly connections.
import { DatabaseSync } from "node:sqlite";
import { existsSync, statSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { log, attempt, tempDir } from "./common.mjs";

const dir = tempDir();
const path = join(dir, "wal.db");
const db = new DatabaseSync(path);
log("journal_mode", db.prepare("PRAGMA journal_mode = WAL").get());
log("synchronous", db.prepare("PRAGMA synchronous = NORMAL").all());
log("mmap", db.prepare("PRAGMA mmap_size = 268435456").get());
log("location", db.location() === path);
log("location main", db.location("main") === path);
log("location temp", db.location("temp"));
log("location missing", db.location("nope"));
db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)");
const insert = db.prepare("INSERT INTO t (v) VALUES (?)");
db.exec("BEGIN");
for (let i = 0; i < 200; i++) insert.run(`row ${i}`);
db.exec("COMMIT");
log("wal exists", existsSync(path + "-wal") && statSync(path + "-wal").size > 0);

const reader = new DatabaseSync(new URL(pathToFileURL(path).href));
log("reader location", reader.location() === path);
db.exec("BEGIN");
insert.run("uncommitted");
log("reader sees committed only", reader.prepare("SELECT count(*) AS n FROM t").get());
log("writer sees own", db.prepare("SELECT count(*) AS n FROM t").get());
db.exec("COMMIT");
log("reader after commit", reader.prepare("SELECT count(*) AS n FROM t").get());
reader.close();

log("checkpoint passive", db.prepare("PRAGMA wal_checkpoint(PASSIVE)").get());
const truncate = db.prepare("PRAGMA wal_checkpoint(TRUNCATE)").all();
log("checkpoint truncate", truncate);
log("wal truncated", statSync(path + "-wal").size);
db.close();
log("wal removed on close", existsSync(path + "-wal"));

const ro = new DatabaseSync(path, { readOnly: true });
log("readonly count", ro.prepare("SELECT count(*) AS n FROM t").get());
log("readonly write", attempt(() => ro.exec("INSERT INTO t (v) VALUES ('x')")));
ro.close();
log("readonly missing", attempt(() => new DatabaseSync(join(dir, "missing.db"), { readOnly: true })));
log("url scheme", attempt(() => new DatabaseSync(new URL("http://example.com/x.db"))));
log("uri mode", attempt(() => new DatabaseSync(`file:${path}?mode=ro`).prepare("SELECT count(*) AS n FROM t").get()));
