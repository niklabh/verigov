#!/bin/sh
# acme-utils entry point. The hermetic builder inlines lib/ above this file and
# substitutes the version/commit placeholders, producing one self-contained script.
ACME_VERSION="__VERSION__"
ACME_COMMIT="__COMMIT__"

acme_banner
case "$1" in
  sum) acme_sum "$2" "$3" ;;
  "") ;;
  *) echo "usage: acme-utils [sum A B]" >&2; exit 2 ;;
esac
