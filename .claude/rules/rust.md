# Rust hygiene

These rules apply to every crate in this workspace. A reviewer rejects a change that breaks one.

## No stringly-typed values

- A closed set of names is an enum. A function parameter that can only hold a few known values
  takes the enum, never `&str`.
- No magic strings. A name that a library or a peer looks up by string (a config key, an
  environment variable, a table or column name, an error code) is declared once as an enum variant
  or a `const` in the module that owns it, and every use refers to that declaration. The same holds
  for magic numbers: a limit, a timeout, a threshold or a length is a named `const` with a unit in
  its name (`ACQUIRE_TIMEOUT_SECS`, `RANDOM_CHARS`).
- Never match on a string to dispatch behaviour. Parse into the enum at the boundary, match on the
  enum inside.

## Database genericity

- The generic flow (naming, run key, lifecycle, keep-on-fail, sweep policy, the macro) holds no
  SQL, SQLSTATE or catalog name of any one database. Every database-specific operation is a method
  of the `Backend` trait, implemented per backend behind its cargo feature.
- Nothing in the library names a consumer project: no URL, credentials, schema path or prefix
  beyond the neutral defaults in `Config::DEFAULT`.

## No needless allocation

- Borrow by default: `&str`, `&[T]`, `&Path` parameters; return `Cow<'_, str>` when a transform
  is usually the identity.
- Every `Vec`, `String`, `HashMap` and `BTreeMap` whose final size is known or bounded is created
  with `with_capacity`. A collection that is filled by a loop over a known-length input is sized
  from that input.
- Never `clone()`, `to_vec()`, `to_owned()` or `to_string()` a value only to return it, pass it on
  or store it once. Move it: `mem::take`, `split_off`, `into_iter`, `Option::take`, destructuring.
  A `clone()` needs a reason a reader can see in the next line.
- No `format!` on a hot path to build a key, a suffix or a query. Compare with `strip_prefix`,
  `strip_suffix`, `starts_with`; build fixed SQL text once in a `const` or `LazyLock`.
- Preallocate scratch buffers outside loops and reuse them.
- No async recursion that boxes per level when a loop with an explicit stack does the same.

## Types and interfaces

- Traits at crate boundaries, so every consumer is testable with a fake. A concrete type from
  another crate crosses a boundary only when it is data.
- Errors are `snafu` enums, one per crate, with context selectors for the variants callers match
  on. No `String` errors, no `Box<dyn Error>` in a public signature.
- Paths are `camino::Utf8PathBuf`/`Utf8Path`.
- Locks: `parking_lot` for state that is never held across an `.await`, `tokio::sync` otherwise.
  Never hold a `parking_lot` guard across an `.await`.
- Public structs expose their fields or a small typed API, not `serde_json::Value` bags.

## Control flow and correctness

- No `unwrap()`/`expect()` outside tests except on an invariant that the surrounding code
  guarantees, and then the `expect` message states that invariant.
- No `unsafe`.
- Closed-set `match` statements are exhaustive; no `_ =>` arm that hides a new variant.
- A background task never swallows an error silently: report it, or return it.
- Every mutation on the database server is idempotent or guarded (a lock, `IF EXISTS`, a
  bookkeeping row), and never forces out a connection it did not open, except when dropping a
  database the harness itself created for the test that just finished.

## Comments and names

- Zero comments by default. A comment states a durable, non-obvious reason: an invariant, a
  workaround with its cause, a footgun. Never "port of X", never a restatement of the code, never a
  situational note about today's failure.
- No doc comments that restate the signature.
- Names say what a thing is, not where it came from.

## Tests

- A test asserts a behaviour or an invariant, not the shape of the implementation. `assert_eq!`
  on the value, not `contains()` on a message.
- Integration tests reach the server only through `DATABASE_URL`; nothing hardcodes a URL, a port
  or credentials. Each test uses its own prefix or bookkeeping schema when it inspects shared
  state, and removes what it created.
