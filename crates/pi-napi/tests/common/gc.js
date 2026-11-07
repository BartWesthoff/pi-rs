'use strict';
// `gcUntil` from Node's test/common, reduced to what these suites use: collect until `check`
// passes. The collection runs as its own job, so yield between rounds — a finalizer registered
// with `napi_add_finalizer` runs from the runtime's job queue, not inside `gc()`.
exports.gcUntil = async function gcUntil(name, check, maxCount = 10) {
  for (let count = 0; count < maxCount; count++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    global.gc();
    if (check()) return;
  }
  throw new Error(`${name} did not happen`);
};
