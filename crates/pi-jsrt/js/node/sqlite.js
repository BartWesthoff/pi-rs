// node:sqlite — DatabaseSync, StatementSync and backup() as in Node 22.23 (src/node_sqlite.cc),
// over the SQLite C API in ops/sqlite.rs. Argument validation, messages and object shapes
// follow Node: prototype methods are enumerable, `isOpen`/`isTransaction` and
// `sourceSQL`/`expandedSQL` are own accessors of each instance, and methods called on a
// foreign receiver throw "Illegal invocation".

function codedError(Ctor, code, message) {
  const err = new Ctor(message);
  err.code = code;
  return err;
}
const invalidArgType = (message) => codedError(TypeError, "ERR_INVALID_ARG_TYPE", message);
const invalidState = (message) => codedError(Error, "ERR_INVALID_STATE", message);
const illegalInvocation = () => new TypeError("Illegal invocation");

const kToken = Symbol("sqlite.token");
// Statement call flags, mirrored in ops/sqlite.rs.
const READ_BIG_INTS = 1;
const RETURN_ARRAYS = 2;
const ALLOW_BARE_NAMED = 4;
const ALLOW_UNKNOWN_NAMED = 8;

// Internal state by instance: connection config and id, statement settings, iterator state.
const databases = new WeakMap();
const statements = new WeakMap();
const iterators = new WeakMap();

function dbState(self) {
  const state = databases.get(self);
  if (state === undefined) throw illegalInvocation();
  return state;
}

function openState(self) {
  const state = dbState(self);
  if (state.id === null) throw invalidState("database is not open");
  return state;
}

function stmtState(self) {
  const state = statements.get(self);
  if (state === undefined) throw illegalInvocation();
  return state;
}

const isObject = (v) => v !== null && (typeof v === "object" || typeof v === "function");
const isInt32 = (v) => typeof v === "number" && (v | 0) === v && !Object.is(v, -0);

// Define methods like V8 templates do: enumerable, writable, configurable, and ahead of
// `constructor`.
function defineMethods(target, methods) {
  const ctor = Object.getOwnPropertyDescriptor(target, "constructor");
  delete target.constructor;
  for (const key of Reflect.ownKeys(methods)) {
    const desc = Object.getOwnPropertyDescriptor(methods, key);
    Object.defineProperty(target, key, { value: desc.value, enumerable: true, writable: true, configurable: true });
  }
  Object.defineProperty(target, "constructor", ctor);
}

function booleanOption(options, name, fallback) {
  const value = options[name];
  if (value === undefined) return fallback;
  if (typeof value !== "boolean") throw invalidArgType(`The "options.${name}" argument must be a boolean.`);
  return value;
}

// `ValidateDatabasePath`: a string or Uint8Array without NUL bytes, or a file: URL (passed to
// SQLite as a URI).
function databaseLocation(path) {
  if (typeof path === "string") {
    if (!path.includes("\0")) return path;
  } else if (path instanceof Uint8Array) {
    if (!path.includes(0)) return path;
  } else if (isObject(path)) {
    const href = path.href;
    if (typeof href === "string" && !href.includes("\0")) {
      if (!href.startsWith("file:")) throw codedError(TypeError, "ERR_INVALID_URL_SCHEME", "The URL must be of scheme file:");
      return href;
    }
  }
  throw invalidArgType('The "path" argument must be a string, Uint8Array, or URL without null bytes.');
}

const isOpenGetter = {
  get isOpen() {
    return dbState(this).id !== null;
  },
};
const isTransactionGetter = {
  get isTransaction() {
    return ops.sqliteInTransaction(openState(this).id);
  },
};
const sqliteType = Symbol.for("sqlite-type");

function DatabaseSync(path, options) {
  if (new.target === undefined) {
    throw codedError(TypeError, "ERR_CONSTRUCT_CALL_REQUIRED", "Cannot call constructor without `new`");
  }
  const config = {
    location: databaseLocation(path),
    readOnly: false,
    enableForeignKeyConstraints: true,
    enableDoubleQuotedStringLiterals: false,
    timeout: 0,
    readBigInts: false,
    returnArrays: false,
    allowBareNamedParameters: true,
    allowUnknownNamedParameters: false,
  };
  let open = true;
  let allowExtension = false;
  if (arguments.length > 1) {
    if (!isObject(options)) throw invalidArgType('The "options" argument must be an object.');
    open = booleanOption(options, "open", open);
    config.readOnly = booleanOption(options, "readOnly", config.readOnly);
    config.enableForeignKeyConstraints = booleanOption(options, "enableForeignKeyConstraints", true);
    config.enableDoubleQuotedStringLiterals = booleanOption(options, "enableDoubleQuotedStringLiterals", false);
    allowExtension = booleanOption(options, "allowExtension", false);
    const timeout = options.timeout;
    if (timeout !== undefined) {
      if (!isInt32(timeout)) throw invalidArgType('The "options.timeout" argument must be an integer.');
      config.timeout = timeout;
    }
    config.readBigInts = booleanOption(options, "readBigInts", false);
    config.returnArrays = booleanOption(options, "returnArrays", false);
    config.allowBareNamedParameters = booleanOption(options, "allowBareNamedParameters", true);
    config.allowUnknownNamedParameters = booleanOption(options, "allowUnknownNamedParameters", false);
  }
  Object.defineProperty(this, "isOpen", { get: Object.getOwnPropertyDescriptor(isOpenGetter, "isOpen").get, enumerable: true });
  Object.defineProperty(this, "isTransaction", {
    get: Object.getOwnPropertyDescriptor(isTransactionGetter, "isTransaction").get,
    enumerable: true,
  });
  this[sqliteType] = "node:sqlite";
  databases.set(this, { id: null, guard: null, config, allowExtension, enableExtension: allowExtension });
  if (open) this.open();
}

function functionOptions(options, flags) {
  for (const name of ["useBigIntArguments", "varargs", "deterministic", "directOnly"]) {
    if (name in flags) flags[name] = booleanOption(options, name, flags[name]);
  }
  return flags;
}

defineMethods(DatabaseSync.prototype, {
  open() {
    const state = dbState(this);
    if (state.id !== null) throw invalidState("database is already open");
    const c = state.config;
    const [id, guard] = ops.sqliteOpen(
      c.location,
      c.readOnly,
      c.timeout,
      c.enableForeignKeyConstraints,
      c.enableDoubleQuotedStringLiterals,
      state.allowExtension,
    );
    state.id = id;
    state.guard = guard;
  },
  close() {
    const state = openState(this);
    ops.sqliteClose(state.id);
    state.id = null;
    state.guard = null;
  },
  prepare(sql) {
    const state = openState(this);
    if (typeof sql !== "string") throw invalidArgType('The "sql" argument must be a string.');
    const [id, guard] = ops.sqlitePrepare(state.id, sql);
    return new StatementSync(kToken, this, id, guard, state.config);
  },
  exec(sql) {
    const state = openState(this);
    if (typeof sql !== "string") throw invalidArgType('The "sql" argument must be a string.');
    ops.sqliteExec(state.id, sql);
  },
  function(name, options, fn) {
    const state = openState(this);
    if (typeof name !== "string") throw invalidArgType('The "name" argument must be a string.');
    let flags = { useBigIntArguments: false, varargs: false, deterministic: false, directOnly: false };
    if (arguments.length >= 3) {
      if (!isObject(options)) throw invalidArgType('The "options" argument must be an object.');
      flags = functionOptions(options, flags);
    } else {
      fn = options;
    }
    if (typeof fn !== "function") throw invalidArgType('The "function" argument must be a function.');
    const argc = flags.varargs ? -1 : fn.length;
    ops.sqliteFunction(state.id, name, fn, argc, flags.deterministic, flags.directOnly, flags.useBigIntArguments);
  },
  location(dbName) {
    const state = openState(this);
    if (dbName !== undefined && typeof dbName !== "string") throw invalidArgType('The "dbName" argument must be a string.');
    return ops.sqliteLocation(state.id, dbName === undefined ? "main" : dbName);
  },
  aggregate(name, options) {
    const state = openState(this);
    if (typeof name !== "string") throw invalidArgType('The "name" argument must be a string.');
    if (!isObject(options)) throw invalidArgType('The "options" argument must be an object.');
    const start = options.start;
    if (start === undefined) throw invalidArgType('The "options.start" argument must be a function or a primitive value.');
    const step = options.step;
    if (typeof step !== "function") throw invalidArgType('The "options.step" argument must be a function.');
    const result = options.result;
    const flags = functionOptions(options, { useBigIntArguments: false, varargs: false, directOnly: false });
    const inverse = options.inverse;
    if (inverse !== undefined && typeof inverse !== "function") {
      throw invalidArgType('The "options.inverse" argument must be a function.');
    }
    let argc = -1;
    if (!flags.varargs) {
      // The first argument of step/inverse is the accumulated value.
      argc = Math.max(step.length - 1, inverse === undefined ? step.length - 1 : inverse.length - 1, 0);
    }
    ops.sqliteAggregate(
      state.id,
      name,
      argc,
      flags.directOnly,
      flags.useBigIntArguments,
      start,
      step,
      inverse,
      typeof result === "function" ? result : undefined,
    );
  },
  createSession() {
    throw codedError(Error, "ERR_METHOD_NOT_IMPLEMENTED", "The createSession() method is not implemented");
  },
  applyChangeset() {
    throw codedError(Error, "ERR_METHOD_NOT_IMPLEMENTED", "The applyChangeset() method is not implemented");
  },
  enableLoadExtension(allow) {
    const state = dbState(this);
    if (typeof allow !== "boolean") throw invalidArgType('The "allow" argument must be a boolean.');
    if (!state.allowExtension && allow) {
      throw invalidState("Cannot enable extension loading because it was disabled at database creation.");
    }
    state.enableExtension = allow;
    if (state.id === null) throw invalidState("database is not open");
    ops.sqliteEnableLoadExtension(state.id, allow);
  },
  loadExtension(path, entryPoint) {
    const state = openState(this);
    if (!state.allowExtension || !state.enableExtension) throw invalidState("extension loading is not allowed");
    if (typeof path !== "string") throw invalidArgType('The "path" argument must be a string.');
    ops.sqliteLoadExtension(state.id, path, typeof entryPoint === "string" ? entryPoint : null);
  },
  [Symbol.dispose]() {
    if (dbState(this).id !== null) this.close();
  },
});
Object.defineProperty(DatabaseSync, "length", { value: 0 });

const sourceSQLGetter = {
  get sourceSQL() {
    return ops.sqliteSourceSql(stmtState(this).id);
  },
};
const expandedSQLGetter = {
  get expandedSQL() {
    return ops.sqliteExpandedSql(stmtState(this).id);
  },
};

function StatementSync(token, db, id, guard, config) {
  if (token !== kToken) throw codedError(Error, "ERR_ILLEGAL_CONSTRUCTOR", "Illegal constructor");
  Object.defineProperty(this, "sourceSQL", { get: Object.getOwnPropertyDescriptor(sourceSQLGetter, "sourceSQL").get, enumerable: true });
  Object.defineProperty(this, "expandedSQL", {
    get: Object.getOwnPropertyDescriptor(expandedSQLGetter, "expandedSQL").get,
    enumerable: true,
  });
  statements.set(this, {
    db, // keeps the connection's JS owner alive while the statement is
    id,
    guard,
    flags:
      (config.readBigInts ? READ_BIG_INTS : 0) |
      (config.returnArrays ? RETURN_ARRAYS : 0) |
      (config.allowBareNamedParameters ? ALLOW_BARE_NAMED : 0) |
      (config.allowUnknownNamedParameters ? ALLOW_UNKNOWN_NAMED : 0),
  });
}

function setFlag(self, flag, enabled, argName) {
  const state = stmtState(self);
  ops.sqliteReset(state.id, false);
  if (typeof enabled !== "boolean") throw invalidArgType(`The "${argName}" argument must be a boolean.`);
  state.flags = enabled ? state.flags | flag : state.flags & ~flag;
}

defineMethods(StatementSync.prototype, {
  iterate(...params) {
    const state = stmtState(this);
    ops.sqliteIterate(state.id, state.flags, ...params);
    const iterator = Object.create(StatementSyncIterator.prototype);
    iterators.set(iterator, { stmt: this, done: false });
    return iterator;
  },
  all(...params) {
    const state = stmtState(this);
    return ops.sqliteAll(state.id, state.flags, ...params);
  },
  get(...params) {
    const state = stmtState(this);
    return ops.sqliteGet(state.id, state.flags, ...params);
  },
  run(...params) {
    const state = stmtState(this);
    return ops.sqliteRun(state.id, state.flags, ...params);
  },
  columns() {
    return ops.sqliteColumns(stmtState(this).id);
  },
  setAllowBareNamedParameters(enabled) {
    setFlag(this, ALLOW_BARE_NAMED, enabled, "allowBareNamedParameters");
  },
  setAllowUnknownNamedParameters(enabled) {
    setFlag(this, ALLOW_UNKNOWN_NAMED, enabled, "enabled");
  },
  setReadBigInts(enabled) {
    setFlag(this, READ_BIG_INTS, enabled, "readBigInts");
  },
  setReturnArrays(enabled) {
    setFlag(this, RETURN_ARRAYS, enabled, "returnArrays");
  },
});

// The iterator of `iterate()`: lazy, one row per `next()`. Like Node, only `return()` ends it;
// `next()` after the last row steps the (reset) statement again.
function StatementSyncIterator() {
  throw codedError(Error, "ERR_ILLEGAL_CONSTRUCTOR", "Illegal constructor");
}
Object.setPrototypeOf(StatementSyncIterator.prototype, Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())));

function iteratorState(self) {
  const state = iterators.get(self);
  if (state === undefined) throw illegalInvocation();
  return state;
}

const iteratorResult = (done, value) => ({ __proto__: null, done, value });

defineMethods(StatementSyncIterator.prototype, {
  next() {
    const iter = iteratorState(this);
    const stmt = statements.get(iter.stmt);
    if (iter.done) {
      ops.sqliteReset(stmt.id, false);
      return iteratorResult(true, null);
    }
    const row = ops.sqliteIterateNext(stmt.id, stmt.flags);
    return row === undefined ? iteratorResult(true, null) : iteratorResult(false, row);
  },
  return() {
    const iter = iteratorState(this);
    ops.sqliteReset(statements.get(iter.stmt).id, true);
    iter.done = true;
    return iteratorResult(true, null);
  },
});

const schedule = typeof setImmediate === "function" ? setImmediate : (fn) => setTimeout(fn, 0);

// `sqlite.backup(sourceDb, destination[, options])`: copies `rate` pages per step, one step
// per event-loop turn, calling `progress` between steps (Node runs the steps on its thread
// pool; the observable order is the same).
function backup(sourceDb, destination, options) {
  if (!isObject(sourceDb)) throw invalidArgType('The "sourceDb" argument must be an object.');
  const state = databases.get(sourceDb);
  if (state === undefined) throw illegalInvocation();
  if (state.id === null) throw invalidState("database is not open");
  if (typeof destination !== "string") throw invalidArgType('The "destination" argument must be a string.');
  let rate = 100;
  let source = "main";
  let target = "main";
  let progress;
  if (arguments.length > 2) {
    if (!isObject(options)) throw invalidArgType('The "options" argument must be an object.');
    if (options.rate !== undefined) {
      if (!isInt32(options.rate)) throw invalidArgType('The "options.rate" argument must be an integer.');
      rate = options.rate;
    }
    if (options.source !== undefined) {
      if (typeof options.source !== "string") throw invalidArgType('The "options.source" argument must be a string.');
      source = options.source;
    }
    if (options.target !== undefined) {
      if (typeof options.target !== "string") throw invalidArgType('The "options.target" argument must be a string.');
      target = options.target;
    }
    if (options.progress !== undefined) {
      if (typeof options.progress !== "function") throw invalidArgType('The "options.progress" argument must be a function.');
      progress = options.progress;
    }
  }
  return new Promise((resolve, reject) => {
    let id;
    try {
      id = ops.sqliteBackupInit(state.id, destination, source, target);
    } catch (err) {
      reject(err);
      return;
    }
    const step = () => {
      let done, totalPages, remainingPages;
      try {
        [done, totalPages, remainingPages] = ops.sqliteBackupStep(id, rate);
      } catch (err) {
        reject(err);
        return;
      }
      if (remainingPages !== 0) {
        if (progress !== undefined) {
          try {
            progress.call(null, { totalPages, remainingPages });
          } catch (err) {
            ops.sqliteBackupFinish(id, false);
            reject(err);
            return;
          }
        }
        schedule(step);
        return;
      }
      try {
        ops.sqliteBackupFinish(id, !done);
      } catch (err) {
        reject(err);
        return;
      }
      resolve(totalPages);
    };
    schedule(step);
  });
}

Object.defineProperty(backup, "length", { value: 2 });
Object.defineProperty(StatementSync, "length", { value: 0 });

internal.defineBuiltin("sqlite", () => ({
  DatabaseSync,
  StatementSync,
  constants: {
    SQLITE_CHANGESET_OMIT: 0,
    SQLITE_CHANGESET_REPLACE: 1,
    SQLITE_CHANGESET_ABORT: 2,
    SQLITE_CHANGESET_DATA: 1,
    SQLITE_CHANGESET_NOTFOUND: 2,
    SQLITE_CHANGESET_CONFLICT: 3,
    SQLITE_CHANGESET_CONSTRAINT: 4,
    SQLITE_CHANGESET_FOREIGN_KEY: 5,
  },
  backup,
}));
