# Multiple ports for an environment component

A single component can reserve named ports for its whole lifecycle:

```toml
[[profiles.components]]
name = "stack"
runtime = "compose"
file = "compose.env.yml"
ports = ["api", "db", "web"]
```

Its commands receive `AETHYME_ENV_PORT_STACK_API`,
`AETHYME_ENV_PORT_STACK_DB`, and `AETHYME_ENV_PORT_STACK_WEB`. All three
ports are reserved together and released when the environment is destroyed.
Names must be unique after conversion to environment variable names. The
existing `listen = true` primary port remains available for a component's
health check and route; named ports are additional reservations.
