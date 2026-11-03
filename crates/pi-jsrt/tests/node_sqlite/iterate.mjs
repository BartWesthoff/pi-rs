// iterate(): lazy stepping (a counting UDF proves rows are produced on demand), iterator
// protocol shapes, return(), the restart after exhaustion, errors mid-iteration, and a
// 1e6-row iteration whose memory stays flat.
import { DatabaseSync } from "node:sqlite";
import { log, attempt } from "./common.mjs";

const db = new DatabaseSync(":memory:");
let produced = 0;
db.function("tick", (x) => (produced++, x));
const rows = db.prepare(`
  WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
  SELECT tick(i) AS i FROM n`);
const it = rows.iterate();
log("tag", Object.prototype.toString.call(it));
log("proto chain", Object.getPrototypeOf(Object.getPrototypeOf(it)) === Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())));
log("own keys", Reflect.ownKeys(it));
log("proto keys", Object.getOwnPropertyNames(Object.getPrototypeOf(it)));
log("constructor", Object.getPrototypeOf(it).constructor.name);
log("self iterable", it[Symbol.iterator]() === it);
const first = it.next();
log("first", [first, Object.getPrototypeOf(first) === null]);
log("produced after one", produced);
let taken = 0;
for (const row of it) {
  if (++taken === 3) break;
}
log("produced after break", produced);
log("after break", it.next());
log("return again", it.return());

const small = db.prepare("SELECT value AS v FROM json_each('[1,2,3]')");
const it2 = small.iterate();
log("drain", [it2.next(), it2.next(), it2.next(), it2.next()]);
log("after exhaustion", [it2.next(), it2.next()]);
log("spread", [...small.iterate()]);
log("arrays", (small.setReturnArrays(true), [...small.iterate()]));
log("two iterators share statement", (() => { small.setReturnArrays(false); const x = small.iterate(); const y = small.iterate(); return [x.next(), y.next(), x.next()]; })());
log("all resets iterator", (() => { const x = small.iterate(); x.next(); small.all(); return x.next(); })());
log("params", [...db.prepare("SELECT ? + value AS v FROM json_each('[1,2]')").iterate(10)]);
log("bind error", attempt(() => db.prepare("SELECT ?").iterate({ nope: 1 })));
db.function("boom", (x) => { if (x === 2) throw new Error("row 2"); return x; });
const boom = db.prepare("SELECT boom(value) AS v FROM json_each('[1,2,3]')").iterate();
log("error mid-iteration", [attempt(() => boom.next()), attempt(() => boom.next()), attempt(() => boom.next())]);
const closing = db.prepare("SELECT 1 AS v").iterate();

const before = process.memoryUsage().rss;
let count = 0;
let sum = 0;
produced = 0;
for (const { i } of rows.iterate()) {
  count++;
  sum += i;
}
const grown = process.memoryUsage().rss - before;
log("1e6 rows", { count, sum, produced });
log("memory flat", grown < 64 * 1024 * 1024);
db.close();
log("next after close", attempt(() => closing.next()));
log("return after close", attempt(() => closing.return()));
log("iterate after close", attempt(() => rows.iterate()));
