#!/bin/sh
# Publishes the packages built by build.mjs: platform packages first, then the main package.
# Skips any package@version already on the registry, so a rerun after a partial failure is safe.
#   sh scripts/npm/publish.sh <out-dir> [extra npm publish args, e.g. --provenance]
set -eu
out=${1:?usage: publish.sh <out-dir> [npm publish args]}; shift
for dir in "$out"/kitbag-*/ "$out"/kitbag/; do
  name=$(node -p "require('$dir/package.json').name")
  version=$(node -p "require('$dir/package.json').version")
  if npm view "$name@$version" version >/dev/null 2>&1; then
    echo "skip $name@$version (already published)"; continue
  fi
  echo "publish $name@$version"
  npm publish "$dir" --access public "$@"
done
