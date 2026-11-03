// exec(): multi-statement scripts, comments, an error part-way (earlier statements stay
// applied), transactions across exec calls, and the argument checks.
import { DatabaseSync } from "node:sqlite";
import { log, attempt } from "./common.mjs";

const db = new DatabaseSync(":memory:");
log("script", db.exec(`
  -- a comment
  CREATE TABLE a (x);
  /* block comment */ INSERT INTO a VALUES (1); INSERT INTO a VALUES (2);
  CREATE TABLE b (y TEXT); INSERT INTO b VALUES ('semi;colon');
  ;;
`));
log("rows", db.prepare("SELECT (SELECT group_concat(x) FROM a) AS a, (SELECT y FROM b) AS b").get());
log("error midway", attempt(() => db.exec("INSERT INTO a VALUES (3); INSERT INTO nope VALUES (1); INSERT INTO a VALUES (4);")));
log("after error", db.prepare("SELECT group_concat(x) AS xs FROM a").get());
log("select in exec", db.exec("SELECT * FROM a"));
log("empty", db.exec(""));
log("whitespace", db.exec("   \n  "));
log("non-string", attempt(() => db.exec(1)));
log("missing", attempt(() => db.exec()));
db.exec("BEGIN");
log("in tx", db.isTransaction);
db.exec("INSERT INTO a VALUES (9)");
db.exec("ROLLBACK");
log("rolled back", db.prepare("SELECT max(x) AS m FROM a").get());
log("commit without tx", attempt(() => db.exec("COMMIT")));
log("pragma", db.exec("PRAGMA user_version = 7"));
log("user_version", db.prepare("PRAGMA user_version").get());
log("prepare takes first", db.prepare("SELECT 1 AS one; SELECT 2 AS two").all());
log("prepare empty", attempt(() => db.prepare("").all()));
log("prepare comment", attempt(() => db.prepare("-- nothing").get()));
db.close();
log("exec closed", attempt(() => db.exec("SELECT 1")));
