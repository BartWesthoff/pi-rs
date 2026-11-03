//! `node:sqlite` (DatabaseSync / StatementSync / backup) against Node 22's observable behavior.
//!
//! The differential scenarios in `tests/node_sqlite/*.mjs` print one line per probe; their
//! `.out` goldens are `/usr/bin/node` 22.23.1's stdout. Each scenario runs under
//! `pi-jsrt-node` and must print the golden byte for byte. When a Node 22.23.x is on `PATH`,
//! `goldens_match_local_node` also re-runs the scenarios under it, so a stale golden fails.
//! Regenerate a golden with `node tests/node_sqlite/<name>.mjs > tests/node_sqlite/<name>.out`.

use std::path::{Path, PathBuf};
use std::process::Command;

use pi_jsrt::{JsRuntime, RuntimeOptions};
use serde_json::json;

const NODE_BIN: &str = env!("CARGO_BIN_EXE_pi-jsrt-node");

const SCENARIOS: &[&str] = &[
    "fts5",
    "wal",
    "busy",
    "exec",
    "savepoints",
    "params",
    "blobs",
    "functions",
    "iterate",
    "api",
];

fn scenario_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/node_sqlite")
}

/// Stdout of `program scenario.mjs`, with stderr kept for failure messages.
fn run_scenario(program: &str, name: &str) -> (String, String) {
    let script = scenario_dir().join(format!("{name}.mjs"));
    let out = Command::new(program)
        .arg(&script)
        .output()
        .unwrap_or_else(|e| panic!("running {program} {}: {e}", script.display()));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{program} {name}.mjs failed ({}):\n{stderr}",
        out.status
    );
    (String::from_utf8_lossy(&out.stdout).into_owned(), stderr)
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(scenario_dir().join(format!("{name}.out"))).unwrap()
}

fn assert_matches_golden(name: &str) {
    let (stdout, stderr) = run_scenario(NODE_BIN, name);
    let want = golden(name);
    for (i, (got, want)) in stdout.lines().zip(want.lines()).enumerate() {
        assert_eq!(
            got,
            want,
            "{name}.mjs line {} differs from Node\nstderr:\n{stderr}",
            i + 1
        );
    }
    assert_eq!(
        stdout.lines().count(),
        want.lines().count(),
        "{name}.mjs line count\nstderr:\n{stderr}"
    );
}

#[test]
fn fts5_matches_node() {
    assert_matches_golden("fts5");
}

#[test]
fn wal_and_checkpoints_match_node() {
    assert_matches_golden("wal");
}

#[test]
fn busy_timeout_across_connections_matches_node() {
    assert_matches_golden("busy");
}

#[test]
fn multi_statement_exec_matches_node() {
    assert_matches_golden("exec");
}

#[test]
fn savepoints_match_node() {
    assert_matches_golden("savepoints");
}

#[test]
fn parameter_binding_matches_node() {
    assert_matches_golden("params");
}

#[test]
fn blobs_match_node() {
    assert_matches_golden("blobs");
}

#[test]
fn user_functions_and_aggregates_match_node() {
    assert_matches_golden("functions");
}

#[test]
fn lazy_iterate_matches_node() {
    assert_matches_golden("iterate");
}

#[test]
fn api_surface_and_backup_match_node() {
    assert_matches_golden("api");
}

/// The goldens are still what the local Node 22.23 prints (skipped without one on `PATH`).
#[test]
fn goldens_match_local_node() {
    let Ok(version) = Command::new("node").arg("--version").output() else {
        return;
    };
    if !String::from_utf8_lossy(&version.stdout).starts_with("v22.23.") {
        return;
    }
    for name in SCENARIOS {
        assert_eq!(
            run_scenario("node", name).0,
            golden(name),
            "node {name}.mjs no longer matches {name}.out"
        );
    }
}

/// context-mode's `loadDatabase()` (`build/db-base.js`) probes `node:sqlite` for FTS5 and picks
/// it over better-sqlite3. Needs the pinned package fixtures (`PI_RS_PKG_FIXTURES`, a directory
/// holding an unpacked `context-mode` package anywhere below it).
#[tokio::test]
#[ignore = "needs PI_RS_PKG_FIXTURES"]
async fn context_mode_fts5_probe_selects_node_sqlite() {
    let root = std::env::var_os("PI_RS_PKG_FIXTURES").expect("PI_RS_PKG_FIXTURES is set");
    let db_base = find_file(Path::new(&root), "context-mode", "build/db-base.js")
        .expect("context-mode/build/db-base.js under PI_RS_PKG_FIXTURES");
    let dir = tempfile::tempdir().unwrap();
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(dir.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let src = format!(
        r#"
        import {{ loadDatabase, nodeSqliteHasFts5, applyWALPragmas, closeDB }} from {url};
        import {{ DatabaseSync }} from "node:sqlite";
        import {{ join }} from "node:path";
        export function run(dir) {{
            const Database = loadDatabase();
            const db = new Database(join(dir, "ctx.db"), {{ timeout: 30000 }});
            applyWALPragmas(db);
            db.exec("CREATE VIRTUAL TABLE chunks USING fts5(title, content)");
            db.prepare("INSERT INTO chunks VALUES (?, ?)").run("t", "searchable context");
            const hits = db.prepare("SELECT title FROM chunks WHERE chunks MATCH ?").all("context");
            const mode = db.pragma("journal_mode");
            const timeout = db.pragma("busy_timeout");
            closeDB(db);
            return {{
                factory: Database.name,
                probe: nodeSqliteHasFts5(DatabaseSync),
                hits: hits.map((h) => h.title),
                mode,
                timeout,
            }};
        }}
    "#,
        url = serde_json::to_string(&format!("file://{}", db_base.display())).unwrap()
    );
    let ns = rt.eval_module(&src, "main.mjs").await.unwrap();
    let out = ns
        .call_method("run", vec![json!(dir.path().display().to_string())])
        .await
        .unwrap();
    assert_eq!(
        out,
        json!({"factory": "NodeDatabaseFactory", "probe": true, "hits": ["t"], "mode": "wal", "timeout": 30000})
    );
}

/// `<dir>/<package>/<rel>` anywhere below `root` (fixtures may be unpacked tarballs or
/// `node_modules` trees).
fn find_file(root: &Path, package: &str, rel: &str) -> Option<PathBuf> {
    let candidate = root.join(package).join(rel);
    if candidate.is_file() {
        return Some(candidate);
    }
    let candidate = root.join("package").join(rel);
    if root.file_name().is_some_and(|n| n == package) && candidate.is_file() {
        return Some(candidate);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir()
            && !entry.file_type().is_ok_and(|t| t.is_symlink())
            && let Some(found) = find_file(&path, package, rel)
        {
            return Some(found);
        }
    }
    None
}

async fn run(src: &str) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(dir.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let ns = rt.eval_module(src, "main.mjs").await.unwrap();
    let out = ns
        .call_method("run", vec![json!(dir.path().display().to_string())])
        .await
        .unwrap();
    drop(dir);
    out
}

#[tokio::test]
async fn database_sync_round_trips_values_and_rows() {
    let out = run(r#"
        import { DatabaseSync, StatementSync } from "node:sqlite";
        export function run() {
            const db = new DatabaseSync(":memory:");
            db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL, data BLOB, big INTEGER)");
            const insert = db.prepare("INSERT INTO t (name, score, data, big) VALUES (?, ?, ?, ?)");
            const first = insert.run("a", 1.5, new Uint8Array([1, 2, 3]), 9007199254740993n);
            insert.run("b", 2, null, 7n);
            const row = db.prepare("SELECT id, name, score, data, typeof(score) AS kind FROM t WHERE name = ?").get("a");
            const missing = db.prepare("SELECT 1 AS one FROM t WHERE name = ?").get("zzz");
            const all = db.prepare("SELECT name FROM t ORDER BY id").all();
            const named = db.prepare("SELECT name FROM t WHERE id = $id").get({ $id: 2 });
            const bare = db.prepare("SELECT name FROM t WHERE id = :id").get({ id: 1 });
            const bigStmt = db.prepare("SELECT big FROM t WHERE id = 1");
            let overflow;
            try { bigStmt.get(); } catch (e) { overflow = e.code; }
            bigStmt.setReadBigInts(true);
            const big = bigStmt.get().big;
            let syntax;
            try { db.prepare("SELEC 1"); } catch (e) { syntax = [e.code, typeof e.errcode, /syntax error/.test(e.message)]; }
            db.exec("BEGIN");
            const inTx = db.isTransaction;
            db.exec("ROLLBACK");
            const columns = db.prepare("SELECT name AS label FROM t").columns().map((c) => c.name);
            db.close();
            let closed;
            try { db.exec("SELECT 1"); } catch (e) { closed = e.code; }
            return {
                first: { changes: first.changes, lastInsertRowid: first.lastInsertRowid },
                proto: Object.getPrototypeOf(row) === null,
                row: { ...row, data: Array.from(row.data), isBytes: row.data instanceof Uint8Array },
                missing: missing === undefined,
                all: all.map((r) => r.name),
                named: named.name,
                bare: bare.name,
                overflow,
                big: typeof big + ":" + String(big),
                syntax,
                inTx,
                columns,
                closed,
                open: db.isOpen,
                statement: insert instanceof StatementSync,
            };
        }
    "#)
    .await;
    assert_eq!(
        out,
        json!({
            "first": {"changes": 1, "lastInsertRowid": 1},
            "proto": true,
            "row": {"id": 1, "name": "a", "score": 1.5, "data": [1, 2, 3], "kind": "real", "isBytes": true},
            "missing": true,
            "all": ["a", "b"],
            "named": "b",
            "bare": "a",
            "overflow": "ERR_OUT_OF_RANGE",
            "big": "bigint:9007199254740993",
            "syntax": ["ERR_SQLITE_ERROR", "number", true],
            "inTx": true,
            "columns": ["label"],
            "closed": "ERR_INVALID_STATE",
            "open": false,
            "statement": true,
        })
    );
}

#[tokio::test]
async fn database_sync_files_read_only_and_foreign_keys() {
    let out = run(r#"
        import { DatabaseSync } from "node:sqlite";
        import { join } from "node:path";
        export function run(dir) {
            const path = join(dir, "x.db");
            const db = new DatabaseSync(path, { timeout: 50 });
            db.exec("CREATE TABLE p (id INTEGER PRIMARY KEY); CREATE TABLE c (p INTEGER REFERENCES p(id)); INSERT INTO p VALUES (1);");
            let fk;
            try { db.prepare("INSERT INTO c VALUES (?)").run(5); } catch (e) { fk = /FOREIGN KEY/.test(e.message); }
            db.close();
            const ro = new DatabaseSync(path, { readOnly: true });
            const count = ro.prepare("SELECT count(*) AS n FROM p").get().n;
            let readOnly;
            try { ro.exec("INSERT INTO p VALUES (2)"); } catch (e) { readOnly = /readonly/.test(e.message); }
            ro.close();
            const lazy = new DatabaseSync(path, { open: false });
            const before = lazy.isOpen;
            lazy.open();
            const after = lazy.isOpen;
            lazy.close();
            let dqs;
            const strict = new DatabaseSync(":memory:");
            try { strict.prepare('SELECT "nope"').get(); } catch (e) { dqs = /no such column/.test(e.message); }
            strict.close();
            return { fk, count, readOnly, before, after, dqs };
        }
    "#)
    .await;
    assert_eq!(
        out,
        json!({"fk": true, "count": 1, "readOnly": true, "before": false, "after": true, "dqs": true})
    );
}

#[tokio::test]
async fn sqlite_is_only_a_node_scheme_builtin() {
    let dir = tempfile::tempdir().unwrap();
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(dir.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let err = rt
        .eval_module("import 'sqlite';", "main.mjs")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("sqlite"), "{err}");
}

/// A collected `DatabaseSync` closes its connection (rolling back its open transaction and
/// releasing the write lock) at the next sqlite call, like Node's weak `BaseObject`.
#[tokio::test]
async fn collected_database_releases_its_connection() {
    let dir = tempfile::tempdir().unwrap();
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(dir.path().to_path_buf()),
        expose_gc: true,
        ..Default::default()
    })
    .unwrap();
    let ns = rt
        .eval_module(
            r#"
            import { DatabaseSync } from "node:sqlite";
            import { join } from "node:path";
            const held = {};
            function insert(db, v) {
                try { db.exec(`INSERT INTO t VALUES (${v})`); return "ok"; } catch (e) { return e.errcode; }
            }
            export function run(dir) {
                const path = join(dir, "gc.db");
                const setup = new DatabaseSync(path);
                setup.exec("PRAGMA journal_mode = WAL; CREATE TABLE t (v INTEGER)");
                held.db = new DatabaseSync(path);
                held.db.exec("BEGIN IMMEDIATE; INSERT INTO t VALUES (1)");
                held.stmt = held.db.prepare("SELECT count(*) AS n FROM t");
                const whileHeld = insert(setup, 2);
                held.db = held.stmt = null;
                gc();
                const afterCollect = insert(setup, 3);
                const rows = setup.prepare("SELECT v FROM t ORDER BY v").all().map((r) => r.v);
                setup.close();
                return { whileHeld, afterCollect, rows };
            }
        "#,
            "main.mjs",
        )
        .await
        .unwrap();
    let out = ns
        .call_method("run", vec![json!(dir.path().display().to_string())])
        .await
        .unwrap();
    assert_eq!(
        out,
        json!({"whileHeld": 5, "afterCollect": "ok", "rows": [3]})
    );
}
