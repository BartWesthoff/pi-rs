// Two connections with busy_timeout: the `timeout` option, PRAGMA busy_timeout, SQLITE_BUSY
// errors (errcode 5) with and without waiting, and recovery after the lock holder commits.
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { log, attempt, tempDir } from "./common.mjs";

const path = join(tempDir(), "busy.db");
const a = new DatabaseSync(path, { timeout: 0 });
a.exec("PRAGMA journal_mode = WAL; CREATE TABLE t (v INTEGER)");
const b = new DatabaseSync(path, { timeout: 150 });
log("timeout pragma a", a.prepare("PRAGMA busy_timeout").get());
log("timeout pragma b", b.prepare("PRAGMA busy_timeout").get());

a.exec("BEGIN IMMEDIATE");
a.prepare("INSERT INTO t VALUES (1)").run();
let start = Date.now();
log("b write while locked", attempt(() => b.prepare("INSERT INTO t VALUES (2)").run()));
const waited = Date.now() - start;
log("b waited for timeout", waited >= 120);
log("b reads while locked", b.prepare("SELECT count(*) AS n FROM t").get());
log("b exec while locked", attempt(() => b.exec("BEGIN IMMEDIATE")));
log("b in transaction", b.isTransaction);

b.exec("PRAGMA busy_timeout = 0");
start = Date.now();
log("b immediate busy", attempt(() => b.prepare("INSERT INTO t VALUES (3)").run()));
log("b failed fast", Date.now() - start < 100);
a.exec("COMMIT");
log("b write after commit", b.prepare("INSERT INTO t VALUES (4)").run());
log("rows", a.prepare("SELECT v FROM t ORDER BY rowid").all());

log("timeout option type", attempt(() => new DatabaseSync(path, { timeout: 1.5 })));
log("timeout option string", attempt(() => new DatabaseSync(path, { timeout: "10" })));
a.close();
b.close();
