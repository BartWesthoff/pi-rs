// Savepoints: nested SAVEPOINT / RELEASE / ROLLBACK TO, isTransaction at each level, and a
// transaction() helper in the style of context-mode's NodeSQLiteAdapter.
import { DatabaseSync } from "node:sqlite";
import { log, attempt } from "./common.mjs";

const db = new DatabaseSync(":memory:");
db.exec("CREATE TABLE t (v INTEGER)");
const add = db.prepare("INSERT INTO t VALUES (?)");
const values = () => db.prepare("SELECT group_concat(v) AS vs FROM t").get().vs;
log("autocommit", db.isTransaction);
db.exec("SAVEPOINT outer");
log("in savepoint", db.isTransaction);
add.run(1);
db.exec("SAVEPOINT inner");
add.run(2);
db.exec("ROLLBACK TO inner");
log("after rollback to inner", values());
add.run(3);
db.exec("RELEASE inner");
log("still in outer", db.isTransaction);
db.exec("RELEASE outer");
log("released", [db.isTransaction, values()]);
log("release unknown", attempt(() => db.exec("RELEASE nope")));

db.exec("BEGIN");
db.exec("SAVEPOINT s1");
add.run(4);
db.exec("ROLLBACK TO s1");
db.exec("RELEASE s1");
db.exec("COMMIT");
log("rolled back inside tx", values());

function transaction(fn) {
  return (...args) => {
    db.exec("BEGIN");
    try {
      const result = fn(...args);
      db.exec("COMMIT");
      return result;
    } catch (e) {
      db.exec("ROLLBACK");
      throw e;
    }
  };
}
const insertMany = transaction((list) => list.map((v) => add.run(v).changes));
log("tx ok", insertMany([5, 6]));
log("tx fails", attempt(() => insertMany([7, "x".repeat(0) || null, (() => { throw new Error("boom"); })()])));
log("after failed tx", [db.isTransaction, values()]);
db.close();
