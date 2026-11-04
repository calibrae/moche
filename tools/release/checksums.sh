#!/bin/bash
# Write <dir>/SHA256SUMS (shasum -a 256 format) for every moche-*.tar.gz in <dir>.
# Usage: tools/release/checksums.sh dist/
set -euo pipefail
D="${1:?usage: $0 <dir>}"
cd "$D"
ls moche-*.tar.gz >/dev/null
LC_ALL=C ls moche-*.tar.gz | LC_ALL=C sort | xargs shasum -a 256 > SHA256SUMS
cat SHA256SUMS
