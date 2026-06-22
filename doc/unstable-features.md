# Unstable features

Unstable features are experimental compiler capabilities. They may change or be removed before stabilization.

## User guide: Viewing available unstable features

You can list all currently available unstable features by running `simc --help`. The features are
displayed in the help output under the `-Z` flag section.

The output looks similar to this:

```text
  -Z, --unstable-feature <FEATURE>  Enable unstable features. Available features:
                                      imports   Module system syntax: 'use' imports, 'mod' modules, 'as' aliases, 'crate::' paths
```

## User guide: Enabling unstable features

To enable an unstable feature, pass the `-Z <feature-name>` flag when compiling with `simc`.

## User guide: Current unstable features

|Feature|Description|
|---|---|
|imports|Enables module system syntax: `use` imports, `mod` modules, `as` import aliases, and `crate::` paths.|

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
children. The check is encapsulated in the parsing module: `parse_from_str_with_errors` collects
the full list right after the AST is successfully built and checks it against the enabled set,
so callers only need to pass the enabled features to the parser.

Every AST node type implements `RequireFeature`. This means adding a new node anywhere in the
tree produces a compile error until the new impl is written, preventing new syntax from silently
bypassing the check.

**Gate nodes** own the syntax that requires a feature. They push a `FeatureRequirement` and
recurse into children:

```rust
impl RequireFeature for UseDecl {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        let UseDecl { file_id: _, visibility: _, path: _, items, span } = self;
        out.push(FeatureRequirement::new(UnstableFeature::Imports, *span));
        items.feature_requirements(out);
    }
}

impl RequireFeature for Module {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        let Module { file_id: _, visibility: _, name: _, items, span } = self;
        out.push(FeatureRequirement::new(UnstableFeature::Imports, *span));
        for item in items.iter() {
            item.feature_requirements(out);
        }
    }
}
```

**Dispatch nodes** forward to their children without knowing which features exist:

```rust
impl RequireFeature for Item {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        match self {
            Item::Use(use_decl) => use_decl.feature_requirements(out),
            Item::Module(module) => module.feature_requirements(out),
            Item::TypeAlias(alias) => alias.feature_requirements(out),
            Item::Function(function) => function.feature_requirements(out),
            Item::Ignored => {}
        }
    }
}
```

The compile-time enforcement conventions that keep these impls exhaustive are documented in
`src/unstable.rs`, on the `RequireFeature` trait and the `impl_require_feature` macro.

**Recurse by default; only `skip:` what cannot carry gated syntax.** The `impl_require_feature`
macro lets a field be either recursed into or listed after `skip:`. A `skip:` is a promise that the
field can never contain feature-gated syntax — and it is unchecked, so a wrong promise silently
defeats the gate. Prefer recursing whenever a field holds another AST node. In particular, types
(`AliasedType`) are traversed, not skipped: no type syntax is gated *today*, but `AliasedType`
implements `RequireFeature` with an exhaustive match so that adding gated type syntax later forces
a decision rather than slipping through.

## Developer guide: Adding a new unstable feature

1. Add a variant to `UnstableFeature`.
2. The compiler will immediately report errors in the exhaustive matches — `Display`,
   `description()`, and the `_check_exhaustive` const fn inside `all()` — so follow those to update
   all three. Three more spots are *not* compiler-enforced on addition and must be updated by hand:
   add the variant to the slice returned by `all()`, add a parse arm to `FromStr` (it matches the
   input string with a `_` catch-all, so a missing arm still compiles but the feature cannot be
   enabled via `-Z`), and bump the count assertion in `all_contains_every_variant`. The
   `test_feature_from_str` and `all_contains_every_variant` tests catch a forgotten `FromStr` arm
   and a stale `all()` slice at test time.
3. Make the feature reachable from the `RequireFeature` traversal, pushing
   `FeatureRequirement::new(UnstableFeature::YourFeature, span)` wherever the gated syntax lives:
   - New AST node (e.g. an `Item` or expression): add its `RequireFeature` impl.
   - Gated syntax inside an *existing* node rather than a new one — most notably type syntax such
     as `crate::`-qualified or imported types — extend that node's existing impl instead of adding
     one. For types this means classifying the new `AliasedType`/`TypeInner` variant in
     `AliasedType::feature_requirements` (in `src/types.rs`); its exhaustive match will not compile
     until you do.
4. If the new syntax introduces a new enum node (e.g. a new `Item` variant), the existing
   exhaustive `match self` in the parent's impl will fail to compile — add the delegate call there.
5. Add tests that compile successfully with the feature enabled and fail with a clear error when
   the feature is disabled.

## Developer guide: Stabilizing an unstable feature

Remove the feature variant from `UnstableFeature`. The compiler will report errors in `Display`,
`FromStr`, `description()`, `all()`, and every `FeatureRequirement::new(...)` call that references
the removed variant — follow the errors to clean up. Tests that are not specifically about feature
gating should use the helpers that enable all features, so they continue to pass after stabilization.
Also remove the `#[allow(dead_code)]` attributes that were added while the feature was unstable
(e.g. the test helpers in `lib.rs` marked "Temporary allow while imports are unstable"): once tests
switch from the `*_with_unstable` helpers back to the plain ones, those allows are no longer needed
and would otherwise hide genuinely dead code.
