# Node's `test/js-native-api` suites (vendored)

These are unmodified copies of Node's `test/js-native-api/*` suites, except for the four files
listed below. `tests/build-fixtures.sh` compiles each suite's addon with the system compiler and
Node's headers; `tests/sync_addons.rs` runs each suite's scripts (one runtime per script) against
the pi-napi table.

Four `test.js` files relax *only* the regexes that match V8's worded `TypeError` messages, because
QuickJS words the same error differently (`'x' is read-only` / `no setter for property` /
`object is not extensible` / `could not delete property`). Every assertion still requires the throw
and the property name; only the sentence shape is accepted in both dialects:

- `6_object_wrap/test.js`, `test_constructor/test.js`, `test_properties/test.js` — getter-only and
  read-only assignment messages.
- `test_object/test.js` — freeze/seal assignment, add and delete messages.

Restoring V8's exact wording would mean rewriting QuickJS's own `TypeError` texts inside the engine,
which is out of scope for the N-API layer.

`test/js-native-api/test_symbol` ships `test1.js`..`test3.js` instead of a single `test.js`; the
runner executes every `.js` file in a suite directory.
