// BLOB values: typed arrays, Buffer, DataView and sub-views bind as bytes; results are fresh
// Uint8Arrays; empty blobs, zeroblob(), x'' literals, and text/blob type affinity.
import { DatabaseSync } from "node:sqlite";
import { log, attempt } from "./common.mjs";

const db = new DatabaseSync(":memory:");
db.exec("CREATE TABLE b (id INTEGER PRIMARY KEY, data BLOB)");
const put = db.prepare("INSERT INTO b (data) VALUES (?)");
const backing = new Uint8Array([9, 8, 7, 6, 5, 4, 3, 2]);
const inputs = {
  uint8: new Uint8Array([1, 2, 3]),
  buffer: Buffer.from("hi"),
  dataview: new DataView(backing.buffer, 2, 3),
  int16: new Int16Array([1, -1]),
  float64: new Float64Array([1.5]),
  subarray: backing.subarray(5),
  empty: new Uint8Array(0),
  uint8clamped: new Uint8ClampedArray([300, -5]),
};
for (const [name, value] of Object.entries(inputs)) log(`put ${name}`, put.run(value));
const rows = db.prepare("SELECT id, data, typeof(data) AS t, length(data) AS n, hex(data) AS h FROM b ORDER BY id").all();
log("rows", rows);
log("result is Uint8Array", rows.every((r) => r.data instanceof Uint8Array && !Buffer.isBuffer(r.data)));
const one = db.prepare("SELECT data FROM b WHERE id = 1").get().data;
one[0] = 99;
log("fresh copy", db.prepare("SELECT hex(data) AS h FROM b WHERE id = 1").get());
log("arraybuffer rejected", attempt(() => put.run(new ArrayBuffer(2))));
log("literals", db.prepare("SELECT x'00ff' AS a, zeroblob(3) AS z, x'' AS e, CAST('ab' AS BLOB) AS c, CAST(x'6869' AS TEXT) AS t").get());
log("blob with nul as text", db.prepare("SELECT CAST(x'610062' AS TEXT) AS t").get());
log("invalid utf8 text", db.prepare("SELECT CAST(x'ff61' AS TEXT) AS t").get());
const big = new Uint8Array(1 << 20).map((_, i) => i & 0xff);
put.run(big);
const back = db.prepare("SELECT data FROM b ORDER BY id DESC LIMIT 1").get().data;
log("1 MiB round trip", back.length === big.length && back.every((v, i) => v === big[i]));
log("blob in named", db.prepare("SELECT hex(:b) AS h").get({ b: Buffer.from([0xca, 0xfe]) }));
log("udf returns view", (() => { db.function("view", () => new DataView(new Uint8Array([4, 5]).buffer)); return db.prepare("SELECT hex(view()) AS h, typeof(view()) AS t").get(); })());
db.close();
