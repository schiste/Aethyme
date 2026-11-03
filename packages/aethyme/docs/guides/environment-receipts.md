# Environment hook receipts

A hook can print `AETHYME_RECEIPT {"port":5432,"database":"copy"}` on a
single line. Later component commands receive the complete JSON object in
`AETHYME_ENV_RECEIPT_DB` when the producing component is named `db`. They
also receive `AETHYME_ENV_DB_PORT=5432` and `AETHYME_ENV_DB_DATABASE=copy`.
These variables can be referenced by a Compose file when its component starts.

Only top-level string, number, and boolean fields with nonempty ASCII
alphanumeric or underscore names are exported individually. Names are
uppercased; strings containing a NUL, nulls, arrays, and objects remain
available only in the JSON receipt. If two fields map to the same variable,
or the name is already reserved by Aethyme, the individual variable is not
exported. The complete receipt remains available. Component `env` values are
literal; they are not expanded by Aethyme.
