// Fixture 2 (CJS): CommonJS exports, loaded natively by jiti.
exports.kind = "cjs";
exports.count = 0;
exports.bump = function bump() {
  exports.count += 1;
  return exports.count;
};
