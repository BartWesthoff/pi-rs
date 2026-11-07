#!/bin/sh
# Compile Node's `test/js-native-api` fixture addons with the system compiler.
#
# They are built once into each suite's `build/Release/` directory, where Node's JS suites
# expect them (`require('./build/' + common.buildType + '/<name>')`). The sources are Node's
# own, vendored unmodified; only the C compiler flags come from here (node-gyp is not needed).
#
#   crates/pi-napi/tests/build-fixtures.sh [suite...]
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$here/js-native-api
inc=${NODE_INCLUDE:-$(dirname -- "$(command -v node 2>/dev/null || echo /usr/bin/node)")/../include/node}
[ -f "$inc/node_api.h" ] || inc=$(dirname -- "$(ls -d "$HOME"/.nvm/versions/node/*/include/node 2>/dev/null | sort -V | tail -1)")
[ -f "$inc/node_api.h" ] || { echo "node_api.h not found; set NODE_INCLUDE" >&2; exit 1; }

cc=${CC:-gcc}
cxx=${CXX:-g++}
# C++ addons need libstdc++ linked in: the pi-rs binary is Rust, so the loader process has no
# C++ runtime for an addon to fall back on.
$cc -shared -fPIC -O1 -I"$inc" -o /tmp/pi-napi-probe.node -x c - <<'EOF'
#include <node_api.h>
napi_value napi_register_module_v1(napi_env env, napi_value exports) { return exports; }
EOF

# build_cc <suite> <name> <define...> -- <source...>
build() {
  suite=$1; name=$2; shift 2
  defines=
  while [ "$1" != "--" ]; do defines="$defines -D$1"; shift; done
  shift
  out=$root/$suite/build/Release
  mkdir -p "$out"
  compiler=$cc
  extra=
  for src in "$@"; do
    case $src in *.cc|*.cpp) compiler=$cxx; extra=-lstdc++ ;; esac
  done
  # shellcheck disable=SC2086
  ( cd "$root/$suite" && $compiler -shared -fPIC -O1 -I"$inc" $defines \
      -DNODE_GYP_MODULE_NAME="$suite" "$@" $extra -o "$out/$name.node" )
}

if [ $# -gt 0 ]; then suites=$*; else suites=$(cd "$root" && ls -d */ | tr -d /); fi
for suite in $suites; do
  case $suite in
    2_function_arguments|3_callbacks|4_object_factory|5_function_factory)
      build "$suite" "$suite" -- "$suite".c ;;
    7_factory_wrap|8_passing_wrapped)
      build "$suite" "$suite" -- "$suite".cc myobject.cc ;;
    test_constructor|test_conversions) build "$suite" "$suite" -- "$suite".c test_null.c ;;
    test_error|test_exception) build "$suite" "$suite" -- "$suite".c ;;
    test_object)
      build test_object test_object NAPI_EXPERIMENTAL -- test_object.c test_null.c
      build test_object test_exceptions NAPI_EXPERIMENTAL -- test_exceptions.c test_null.c ;;
    test_properties|test_symbol) build "$suite" "$suite" -- "$suite".c ;;
    test_reference)
      build test_reference test_reference -- test_reference.c
      build test_reference test_finalizer -- test_finalizer.c ;;
    test_string) build test_string test_string NAPI_VERSION=10 NAPI_EXPERIMENTAL -- test_string.c test_null.c ;;
    test_typedarray)
      build test_typedarray test_typedarray -- test_typedarray.c
      build test_typedarray test_typedarray_sharedarraybuffer -- test_typedarray_sharedarraybuffer.c ;;
    *) ;;
  esac
done

# The object-wrap suites build extra addons from their own headers.
build 6_object_wrap myobject -- myobject.cc
build 6_object_wrap myobject_basic_finalizer NAPI_EXPERIMENTAL -- myobject.cc
build 6_object_wrap nested_wrap NAPI_VERSION=10 -- nested_wrap.cc
build 7_factory_wrap myobject -- myobject.cc
build 8_passing_wrapped myobject -- myobject.cc
