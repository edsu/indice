---
title: Making a forgotten check a compile error
description: Witness types — proof-carrying arguments that a permission check actually ran.
---

The previous chapter made a *value* impossible to construct wrongly. This one
does the same to an *event*: proving that a check happened.

The usual way to gate a web handler is to call something at the top:

```rust
// the shape indice does NOT use
async fn delete_crawl(state: State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !may_curate(&state, &headers) { return forbidden(); }
    // …
}
```

This works, and it fails in one specific way: the next handler someone adds
forgets the first line. Nothing breaks, nothing warns, and the endpoint is
simply open. You find out later.

## A type as proof

```rust
pub(super) struct Curator(Principal);
pub(super) struct Admin(Principal);
```

Two newtypes again, and again the field is private — this time to `server::auth`.
Nothing outside that module can construct one. The only way to obtain a `Curator`
is to be *given* one, and the only thing that hands them out is the extractor:

```rust
impl axum::extract::FromRequestParts<Arc<AppState>> for Curator {
    type Rejection = Denied;
    async fn from_request_parts(/* … */) -> Result<Self, Self::Rejection> {
        require(state, &parts.headers, Role::Curator, "curating this archive")
            .map(Curator)
    }
}
```

axum runs this for any handler argument of that type; if `require` fails the
handler never runs. So a handler declares its own privilege in its signature:

```rust
pub(super) async fn create_annotation(
    State(state): State<Arc<AppState>>,
    curator: Curator,
    // …
) -> Response
```

Holding a `Curator` *is* the proof that the check ran. There is no other way to
have one, so there is no way to forget.

The pattern is called a **witness type**, or a capability: a value whose
existence testifies that something was established. It costs nothing at runtime —
it wraps a `Principal` the handler needed anyway — and it moves "remember to
check" out of human discipline and into the type system.

## The version that looks silly and is not

```rust
fn new_import_job(_curator: &Curator, state: &Arc<AppState>) -> u64 {
    state.job_counter.fetch_add(1, Ordering::Relaxed)
}
```

The underscore says it: this function never reads the `Curator`. So why take one?

Because taking it means it cannot be *called* without one. Starting an import
spends the operator's own Browsertrix or Archive-It credentials, which is exactly
the kind of thing that should not be reachable from a handler where someone
forgot an extractor. The parameter is not data, it is a precondition, and the
compiler enforces it.

`start_index_job` has the same shape, with one difference worth noting: it does
read its `Curator`, for `principal().id()`, because a crawl records who
accessioned it. Same argument, two jobs — the proof, and the identity.

When an argument exists only to constrain who may call a function, `_name` plus a
comment saying so is the honest way to write it.

## Roles, ordered

```rust
Role::Reader < Role::Curator < Role::Admin
```

A fieldless enum deriving `Ord`, so a check is one comparison and each role
contains the one below it. Two witness types rather than one
`requires(role: Role)` parameter, because two is the number there are — and a
type per role puts the privilege in the signature instead of in an argument you
have to read.

The rule it encodes is one sentence: *curators add and can undo their own
additions; only admins remove a collection.*

## A third type, for a different question

```rust
enum Evidence { Loopback, Proxy, Cookie }
```

Who you are and *how we know* are separate questions. indice sets a display-only
cookie so ungated public pages can show workroom chrome to a signed-in curator.
That cookie must never authorize a write, because a cookie is precisely what a
cross-site request brings along for free.

Making that an enum rather than a comment puts the rule — `Cookie` renders, it
does not authorize — somewhere the compiler helps: a `match` that gains an arm,
or forgets one, is a build error rather than a silent hole.

## The limit

A witness type proves a check *ran*. It cannot prove it was the *right* one:
`Curator` on a handler that should have demanded `Admin` compiles happily.
indice covers that with a test that walks every management route and asserts the
privilege each one demands — the kind of thing types cannot do for you.

That boundary is worth holding onto. These patterns move a whole class of mistake
from runtime to compile time, and then you still write tests for the part that is
a judgement call.

## What to take forward

- A private field plus a single constructor turns "this was checked" into a value
  you can pass around.
- An unused parameter can be a precondition; `_name` and a comment say so.
- Enums make the cases exhaustive, so adding one breaks the code that needs
  updating.
- Types catch forgetting. Tests catch choosing wrong.
