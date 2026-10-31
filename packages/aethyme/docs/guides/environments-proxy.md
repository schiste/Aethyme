# Reaching environments: the built-in proxy and external proxies

Last Updated: 2026-10-31

Every running `aethyme env supervise` process serves a small HTTP/1.1
reverse proxy on loopback, `127.0.0.1:4180` by default
(`AETHYME_ENV_PROXY_PORT` changes it). It routes by the `Host` header:

| Host | Goes to |
|---|---|
| `<component>.<namespace>.<route_domain>` | that routed component |
| `<namespace>.<route_domain>` | the profile's first routed component |

`aethyme env routes` lists the current table; `aethyme env up --json` prints
an environment's namespace.

## What the proxy does

- **Active** environment: the request is forwarded to the component's
  reserved port. After the request head, bytes are copied both ways, so
  WebSocket upgrades work.
- **Parked** environment: it is woken, and the request is **held** until it
  is healthy (bounded by the components' health timeouts). If the wake fails,
  the answer is `503` with `Retry-After`, the environment id and
  `aethyme env logs <id>`.
- **Static** component (`runtime = "static"`): files under `serve_dir` are
  served directly, with no process and no capacity units.
- Proxied requests count as activity, so an environment someone is
  browsing is not parked as idle.

Routing is decided per connection, from its first request's `Host`.

## Several services on one address

A routed component can send some paths elsewhere, so one address serves a
built app and forwards its API and WebSocket traffic (#1296):

```toml
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
paths = [
  { prefix = "/api", to = "api" },             # another routed component
  { prefix = "/ws", to = "shared:realtime" },  # a shared environment in uses_shared
  { prefix = "/legacy", to = "http://127.0.0.1:9000" },
]
```

The longest matching prefix wins, and the prefix is kept on the forwarded
request (`/api/users` reaches the api component as `/api/users`). `/api`
matches `/api`, `/api/...` and `/api?...`, never `/apix`. A component or
shared target is held and woken like a host route. An `http://` target
receives its own `Host` header; TLS upstreams (`https://`) are not supported.
Paths not under any prefix go to the component itself.

## Hostnames and `.localhost`

The default `route_domain = "localhost"` relies on RFC 6761: browsers
(Chrome, Firefox, Safari) and curl resolve any `*.localhost` name to
loopback, so `http://web.<namespace>.localhost:4180/` works in a browser
with no DNS setup. Some resolvers (for example `getaddrinfo` in some
language runtimes) do not resolve subdomains of `localhost`. For those,
connect to the proxy directly and pass the host:

```sh
curl -H "Host: web.<namespace>.localhost" http://127.0.0.1:4180/
```

## Ports 80 and 443, TLS: put an external proxy in front

The built-in proxy listens only on an unprivileged loopback port and does
not terminate TLS. To serve environments on `:80`/`:443`, or under a real
domain, run an external proxy that forwards to it. The external proxy only
forwards: holding and waking stay in Aethyme, so no proxy plugin is needed.

### Caddy (reference configuration)

```caddyfile
# Caddyfile: every *.localhost name goes to the Aethyme proxy, which wakes
# parked environments and holds the request until they are healthy.
http://*.localhost {
	reverse_proxy 127.0.0.1:4180 {
		# Keep the original Host: it selects the environment.
		header_up Host {host}
		# A wake can take as long as the slowest health check.
		transport http {
			response_header_timeout 120s
		}
	}
}
```

Other proxies (Traefik, nginx) work the same way: forward the original
`Host` to `127.0.0.1:4180` and allow a response timeout at least as long as
your slowest health check. None of these external configurations, Caddy
included, is exercised by Aethyme's test suite; the built-in proxy's
behaviour they rely on (hold, wake, `503`, routes) is.

## Control endpoints

For scripts and external tooling:

- `GET /__aethyme/routes`: the route table as JSON
  (`schema: aethyme.env-routes/v1`) with an `ETag`; send `If-None-Match`
  to receive `304` when nothing changed. A route has an `upstream` port only
  while its environment is active.
- `POST /__aethyme/wake/<namespace>`: wake an existing environment and
  answer when it is healthy (`200`) or failed (`503`). It runs repository
  commands, so it requires `Authorization: Bearer <token>`, where the token
  is the content of `<host state>/environments/proxy.token` (owner-only).
  An unknown namespace is `404`: the endpoint never creates an
  environment, and it only ever runs the commands recorded when the
  environment was brought up.
