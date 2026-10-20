# Test-only signing key

`dev-key.pem` is an RSA key made for tests and for `its4tasks-dev-auth`, the
local stand-in for a real sign-in service. `dev-jwks.json` is its public half
as a JWKS (key id `its4tasks-dev`).

It is public by being in this repository: never configure a real server to
trust it.
