// FTS5: MATCH, rank ordering, bm25(), snippet(), highlight(), prefix and phrase queries,
// column filters, the external-content pattern and the trigram tokenizer.
import { DatabaseSync } from "node:sqlite";
import { log, attempt } from "./common.mjs";

const db = new DatabaseSync(":memory:");
db.exec(`
  CREATE VIRTUAL TABLE docs USING fts5(title, body, tokenize = 'porter unicode61');
  INSERT INTO docs (title, body) VALUES
    ('SQLite FTS5', 'Full-text search in SQLite uses virtual tables and ranking functions.'),
    ('Rust ports', 'Porting a TypeScript harness to Rust keeps the packages unchanged.'),
    ('Searching', 'Searches rank documents: the best match for searching comes first.'),
    ('Unrelated', 'Nothing to see here.');
`);
const q = db.prepare("SELECT rowid, title FROM docs WHERE docs MATCH ? ORDER BY rank");
log("match search", q.all("search"));
log("match prefix", q.all("rank*"));
log("match phrase", q.all('"virtual tables"'));
log("match column", q.all("title:rust"));
log("match boolean", q.all("search NOT sqlite"));
log("match none", q.all("zebra"));
log("bm25", db.prepare("SELECT rowid, round(bm25(docs), 4) AS score FROM docs WHERE docs MATCH 'search' ORDER BY rank").all());
log("rank column", db.prepare("SELECT rowid, round(rank, 4) AS rank FROM docs WHERE docs MATCH 'rust OR search' ORDER BY rank").all());
log("snippet", db.prepare("SELECT snippet(docs, 1, '[', ']', '…', 6) AS s FROM docs WHERE docs MATCH 'ranking' ").get());
log("highlight", db.prepare("SELECT highlight(docs, 0, '<b>', '</b>') AS h FROM docs WHERE docs MATCH 'title:sqlite'").get());
log("syntax error", attempt(() => q.all('"unterminated')));
log("unknown column", attempt(() => q.all("nope:x")));

db.exec(`
  CREATE TABLE notes (id INTEGER PRIMARY KEY, text TEXT);
  CREATE VIRTUAL TABLE notes_fts USING fts5(text, content = 'notes', content_rowid = 'id');
  CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts (rowid, text) VALUES (new.id, new.text);
  END;
  CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts (notes_fts, rowid, text) VALUES ('delete', old.id, old.text);
  END;
`);
const add = db.prepare("INSERT INTO notes (text) VALUES (?)");
for (const t of ["alpha beta", "beta gamma", "gamma delta"]) add.run(t);
db.prepare("DELETE FROM notes WHERE id = ?").run(2);
log("external content", db.prepare("SELECT n.id, n.text FROM notes_fts f JOIN notes n ON n.id = f.rowid WHERE notes_fts MATCH ? ORDER BY rank").all("gamma"));
db.exec("INSERT INTO notes_fts (notes_fts) VALUES ('optimize')");
log("integrity", attempt(() => db.exec("INSERT INTO notes_fts (notes_fts, rank) VALUES ('integrity-check', 1)")));

db.exec("CREATE VIRTUAL TABLE tri USING fts5(t, tokenize = 'trigram')");
db.prepare("INSERT INTO tri VALUES (?)").run("context-mode keeps sessions searchable");
log("trigram", db.prepare("SELECT t FROM tri WHERE tri MATCH ?").all("sion"));
log("trigram like", db.prepare("SELECT count(*) AS n FROM tri WHERE t LIKE '%earch%'").get());
log("vocab", db.prepare("SELECT count(*) > 0 AS ok FROM pragma_module_list WHERE name = 'fts5vocab'").get());
db.close();
