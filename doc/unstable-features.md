# Unstable features

Unstable features are experimental compiler capabilities. They may change or be removed before stabilization.

## User guide: Viewing available unstable features

You can list all currently available unstable features by running `simc --help`. The features are
displayed in the help output under the `-Z` flag section.

The output looks similar to this:

```text
  -Z, --unstable-feature <FEATURE>  Enable unstable features. Available features:
                                      imports   Import syntax: 'use', 'crate::', and import aliases
```

## User guide: Enabling unstable features

To enable an unstable feature, pass the `-Z <feature-name>` flag when compiling with `simc`.

## User guide: Current unstable features

|Feature|Description|
|---|---|
|imports|Enables import and module syntax, including `use`, `crate::`, and import aliases.|

## Developer guide: How unstable features are checked

After parsing and before any further compilation, every AST node reports which unstable features
it uses. If a reported feature is not enabled, the compiler emits an error. This check is
implemented via the `RequireFeature` trait:

```rust
pub(crate) trait RequireFeature {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>);
}
```

Each node appends a `FeatureRequirement` (carrying the feature name and source span) for every
feature-gated construct it directly contains, then calls `feature_requirements` on each of its
children. The call site collects the full list and checks it against the enabled set:

```rust
unstable_features.check_program(&program, &source, &mut handler);
```

Every AST node type implements `RequireFeature`, almost always via the derive macro from the
`simplicityhl-derive` crate:

```rust
#[derive(Clone, Debug, RequireFeature)]
pub struct Function { ... }
```

The derived impl recurses into **every** field (and every enum variant payload), so each field's
type must itself implement `RequireFeature`. This is the central forcing function: adding a new
node type anywhere in the tree, or a new field of a type the traversal has never seen, is a
compile error until the feature requirements of that type are decided. Container shapes
(`Arc<T>`, slices, `Vec<T>`, `Option<T>`, tuples, `Either<L, R>`) are covered once by blanket
impls in `src/unstable.rs` and compose automatically (e.g. `Option<Arc<Expression>>`).

**Gate nodes** own the syntax that requires a feature. They declare it with an attribute, which
pushes a `FeatureRequirement` attached to the node's `span` field before recursing into children:

```rust
#[derive(Clone, Debug, RequireFeature)]
#[require_feature(requires(Imports))]
pub struct UseDecl { ..., span: Span }
```

`UseDecl` and `Module` are the gate nodes for the `imports` feature. Grepping for
`require_feature(requires` lists every gating site.

**Dispatch nodes** (everything else) just derive: the generated impl forwards to all children
without knowing which features exist.

Two kinds of impl are written by hand, both in service of exhaustiveness:

- **Atoms** that can never contain feature-gated syntax (names, literals, numbers, `Span`) get a
  no-op impl through the `impl_require_feature_never!` list in `src/unstable.rs`. Listing a type
  there is a per-type promise — recorded once, in one greppable place — that no part of its
  grammar will ever be gated.
- **Boundary types** whose grammar could plausibly gain gated syntax later keep an exhaustive
  handwritten impl. `AliasedType` (in `src/types.rs`) is the example: no type syntax is gated
  today, but its impl matches every `TypeInner` variant so that a future feature-gated type form
  (e.g. `crate::`-qualified aliases) is a compile error until its gating is decided.

There is also a `#[require_feature(skip)]` field/variant attribute that excludes a field from
recursion. Prefer the `impl_require_feature_never!` list instead: a `skip` is the same unchecked
promise, but scattered per use site rather than recorded per type.

**Compile-time enforcement**, summarized:

- A new node type or field of an un-traversed type fails the derive with
  `RequireFeature is not satisfied`.
- A new enum variant is included in the derived traversal automatically.
- `UnstableFeature::all()` contains a `_check_exhaustive` const fn that exhaustively matches
  every variant, ensuring the returned slice stays in sync with the enum.

`UnstableFeatures` stores enabled features in a `Vec`. A linear scan is fast enough given the
small number of features that will ever exist.

## Developer guide: Adding a new unstable feature

1. Add a variant to `UnstableFeature`.
2. The compiler will immediately report errors in `Display`, `FromStr`, `description()`, and the
   `_check_exhaustive` const fn inside `all()` — update all four, and add the new variant to the
   slice returned by `all()`.
3. Mark the AST nodes that own the gated syntax with
   `#[require_feature(requires(YourFeature))]` (the node needs a `span` field). For a new node
   type, also add `RequireFeature` to its derive list — the parent's derived traversal will not
   compile until you do.
4. If the gated syntax lives inside an *existing* hand-written impl (e.g. a new `TypeInner`
   variant in `AliasedType`), its exhaustive match will not compile until the new variant's
   gating is decided there.
5. Add tests that compile successfully with the feature enabled and fail with a clear error when
   the feature is disabled.

## Developer guide: Stabilizing an unstable feature

Remove the feature variant from `UnstableFeature`. The compiler will report errors in `Display`,
`FromStr`, `description()`, `all()`, and every `#[require_feature(requires(...))]` attribute that
references the removed variant — follow the errors to clean up. Tests that are not specifically about feature
gating should use the helpers that enable all features, so they continue to pass after stabilization.
