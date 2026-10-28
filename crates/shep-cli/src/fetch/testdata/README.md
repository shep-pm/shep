# TLS fixtures

Committed PEM fixtures for `crates/shep-cli/src/fetch/tests/tls.rs`: a
throwaway CA, a leaf it signed, and a self-signed leaf. All three are named
`shep.test`, which no public CA issues, and valid 2026-01-01 to 2038-01-01.

`ca.pem` is the CA certificate. `private-leaf.pem` chains to it.
`selfsigned-leaf.pem` chains to itself. The two `*.key.pem` files are the
keys the test server presents. The CA's own key is not committed: the
regeneration below makes a new CA and re-signs everything under it, so the
fixtures turn over as a set.

## Regenerating

Run from an empty directory and copy the five outputs here.

```bash
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout ca.key.pem -not_before 20260101000000Z -not_after 20380101000000Z \
  -subj "/CN=shep test fixture CA" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" -out ca.pem

openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout private-leaf.key.pem -subj "/CN=shep.test" -out leaf.csr
printf 'basicConstraints=CA:FALSE\nsubjectAltName=DNS:shep.test\nextendedKeyUsage=serverAuth\n' > leaf.ext
openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key.pem -set_serial 0x2a \
  -not_before 20260101000000Z -not_after 20380101000000Z \
  -extfile leaf.ext -out private-leaf.pem

openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout selfsigned-leaf.key.pem -not_before 20260101000000Z -not_after 20380101000000Z \
  -subj "/CN=shep.test" \
  -addext "basicConstraints=CA:FALSE" \
  -addext "subjectAltName=DNS:shep.test" \
  -addext "extendedKeyUsage=serverAuth" -out selfsigned-leaf.pem
```

`ca.key.pem` and `leaf.csr` stay behind.
