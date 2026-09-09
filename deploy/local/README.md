# Local HTTPS edge

Run Caddy with `caddy run --config deploy/local/Caddyfile --adapter caddyfile`.
Set `WISP_EDGE_ADDRESS` to an HTTPS localhost address, `WISP_UPSTREAM` to the
local application's HTTP address, and `WISP_EDGE_CERT` / `WISP_EDGE_KEY` to the
local certificate and key paths. The listener binds only to loopback.

Validated with Caddy 2.11.4: HTTP/1.1, HTTP/2 and HTTP/3 uploads receive streamed
responses before upload EOF. `enable_full_duplex` is necessary for that behavior
on HTTP/1.1. This configuration uses supplied certificates; it does not use ACME.
