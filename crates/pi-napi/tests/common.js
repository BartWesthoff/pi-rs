// Stand-in for Node's `test/common` in this fixture tree: only the helpers the vendored
// `test/js-native-api` suites use (buildType plus the call assertions).
'use strict';

exports.buildType = 'Release';

let expected = 0;
exports.mustCall = function mustCall(fn, n = 1) {
  if (fn === undefined) {
    expected += n;
    return;
  }
  expected += n;
  return function (...args) {
    expected -= 1;
    return fn.apply(this, args);
  };
};
exports.mustCallAtLeast = exports.mustCall;
exports.mustNotCall = function mustNotCall() {
  return function () {
    throw new Error('must not be called');
  };
};
exports.nodeProcessAborted = function nodeProcessAborted() {
  return true;
};
exports.isAIX = false;
