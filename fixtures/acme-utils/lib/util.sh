# acme-utils: shared helpers
acme_banner() {
  echo "acme-utils ${ACME_VERSION} (commit ${ACME_COMMIT})"
}

acme_sum() {
  echo $(( $1 + $2 ))
}
