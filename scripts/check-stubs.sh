#!/usr/bin/env bash
# Stub gate: no `todo!()` body and no `// WPxx-STUB` header anywhere in the tree. (The
# work-package status table this once read is gone; every package landed.) bash 3.2 compatible.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

status=0
if hits="$(grep -rnE '\btodo!\(' src tests examples --include='*.rs')"; then
    echo "check-stubs: todo!() bodies:" >&2
    echo "$hits" >&2
    status=1
fi
if hits="$(grep -rnE '// WP[0-9]{2}-STUB' src tests examples --include='*.rs')"; then
    echo "check-stubs: stub headers:" >&2
    echo "$hits" >&2
    status=1
fi
[[ $status -eq 0 ]] && echo "check-stubs: OK"
exit $status
