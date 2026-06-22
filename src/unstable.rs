use std::fmt;
use std::str::FromStr;

use crate::error::{Error, ErrorCollector, RichError, Span};
use crate::source::SourceFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnstableFeature {
    /// Import and module syntax: `use` declarations, modules declared with the
    /// `mod` keyword, import aliases declared with the `as` keyword, and
    /// `crate::` paths.
    ///
    /// All of this syntax is gated behind a single feature because it forms one
    /// module system: modules are only reachable through imports, and aliases and
    /// `crate::` paths only occur inside `use` declarations.
    Imports,
}

impl fmt::Display for UnstableFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Imports => write!(f, "imports"),
        }
    }
}

impl FromStr for UnstableFeature {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "imports" => Ok(UnstableFeature::Imports),
            _ => Err(format!("Unknown unstable feature: '{s}'")),
        }
    }
}

impl UnstableFeature {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Imports => {
                "Module system syntax: 'use' imports, 'mod' modules, 'as' aliases, 'crate::' paths"
            }
        }
    }

    pub fn all() -> &'static [UnstableFeature] {
        // Exhaustive match forces a compile error when a new variant is added,
        // ensuring the slice below stays in sync.
        const fn _check_exhaustive(f: UnstableFeature) {
            match f {
                UnstableFeature::Imports => {}
            }
        }
        &[Self::Imports]
    }

    pub fn help_message() -> String {
        let max_len = Self::all()
            .iter()
            .map(|feature| feature.to_string().len())
            .max()
            .unwrap_or(0);

        let lines: Vec<String> = Self::all()
            .iter()
            .map(|feature| {
                format!(
                    "  {name:<width$} {desc}",
                    name = feature.to_string(),
                    width = max_len + 2,
                    desc = feature.description()
                )
            })
            .collect();
        format!(
            "Enable unstable features. Available features:\n{}",
            lines.join("\n")
        )
    }
}

pub(crate) struct FeatureRequirement {
    feature: UnstableFeature,
    span: Span,
}

impl FeatureRequirement {
    pub(crate) fn new(feature: UnstableFeature, span: Span) -> Self {
        Self { feature, span }
    }
}

/// Implemented by parsed syntax nodes that can require unstable compiler features.
///
/// Each node pushes every feature-gated construct it owns (and recursively its
/// children's) into `out`. The caller then checks those uses against the enabled
/// feature set via [`UnstableFeatures::check_program`].
///
/// Implementations must be exhaustive so that new syntax cannot silently bypass
/// the check: enum impls match on `self` without a wildcard arm (adding a variant
/// is a compile error until its arm is written), and struct impls destructure
/// every field (adding a field is a compile error until the pattern is updated),
/// preferably via [`impl_require_feature`].
pub(crate) trait RequireFeature {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>);
}

impl<T: RequireFeature + ?Sized> RequireFeature for std::sync::Arc<T> {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        self.as_ref().feature_requirements(out);
    }
}

impl<T: RequireFeature> RequireFeature for [T] {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        for item in self {
            item.feature_requirements(out);
        }
    }
}

impl<T: RequireFeature> RequireFeature for Option<T> {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        if let Some(inner) = self {
            inner.feature_requirements(out);
        }
    }
}

impl<L: RequireFeature, R: RequireFeature> RequireFeature for either::Either<L, R> {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        match self {
            either::Either::Left(inner) => inner.feature_requirements(out),
            either::Either::Right(inner) => inner.feature_requirements(out),
        }
    }
}

/// Implement [`RequireFeature`] for a struct by exhaustively destructuring it.
///
/// Every field must be listed exactly once: in the leading list to recurse into,
/// or after `skip:` if it cannot contain feature-gated syntax. Adding a field to
/// the struct without classifying it here is a compile error, so feature-gated
/// syntax cannot silently bypass the check.
///
/// Nodes that introduce a [`FeatureRequirement`] themselves (rather than only
/// forwarding their children's) must implement the trait by hand.
macro_rules! impl_require_feature {
    // All-skip arm: a struct whose every field is incapable of carrying
    // feature-gated syntax. Kept for symmetry with the recurse arm so such a
    // struct can still opt in to the exhaustive-destructure guard.
    ($ty:ident; skip: $($skip:ident),* $(,)?) => {
        impl $crate::unstable::RequireFeature for $ty {
            fn feature_requirements(
                &self,
                _out: &mut Vec<$crate::unstable::FeatureRequirement>,
            ) {
                let $ty { $($skip: _,)* } = self;
            }
        }
    };
    ($ty:ident; $($recurse:ident),+ $(; skip: $($skip:ident),+ $(,)?)?) => {
        impl $crate::unstable::RequireFeature for $ty {
            fn feature_requirements(
                &self,
                out: &mut Vec<$crate::unstable::FeatureRequirement>,
            ) {
                let $ty { $($recurse,)+ $($($skip: _,)+)? } = self;
                $($recurse.feature_requirements(out);)+
            }
        }
    };
}
pub(crate) use impl_require_feature;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnstableFeatures {
    enabled_features: Vec<UnstableFeature>,
}

impl UnstableFeatures {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn all() -> Self {
        Self::new(UnstableFeature::all().iter().copied())
    }

    pub fn new(features: impl IntoIterator<Item = UnstableFeature>) -> Self {
        let mut enabled_features: Vec<UnstableFeature> = features.into_iter().collect();
        enabled_features.sort_unstable();
        enabled_features.dedup();
        Self { enabled_features }
    }

    fn is_enabled(&self, feature: UnstableFeature) -> bool {
        self.enabled_features.contains(&feature) // linear scan; n is always tiny
    }

    pub(crate) fn check_program(
        &self,
        program: &impl RequireFeature,
        source: &SourceFile,
        handler: &mut ErrorCollector,
    ) {
        let mut uses = Vec::new();
        program.feature_requirements(&mut uses);
        for req in uses {
            if !self.is_enabled(req.feature) {
                let error = Error::UnstableFeature {
                    feature: req.feature,
                };
                handler.push(RichError::new(error, req.span).with_source(source.clone()));
            }
        }
    }

    pub fn from_names(names: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Self, String> {
        let features = names
            .into_iter()
            .filter_map(|n| {
                let s = n.as_ref().trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.parse::<UnstableFeature>())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Route through `new` so the sorted + deduped invariant holds regardless
        // of constructor (e.g. `-Z imports -Z imports` collapses to one entry).
        Ok(Self::new(features))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::source::SourceFile;

    #[test]
    fn test_feature_display() {
        for feature in UnstableFeature::all() {
            let name = feature.to_string();
            assert!(!name.is_empty());
            assert!(!name.contains(' '));
        }
    }

    #[test]
    fn test_feature_descriptions() {
        for feature in UnstableFeature::all() {
            assert!(!feature.description().is_empty());
        }
    }

    #[test]
    fn all_contains_every_variant() {
        let all_features = UnstableFeature::all();

        // When you add a variant to `UnstableFeature`, bump this count and add
        // the variant to `all()`. Pins that `all()` stays complete.
        assert_eq!(
            all_features.len(),
            1,
            "update this count when adding a feature"
        );

        let mut unique = HashSet::new();
        for feature in all_features {
            assert!(
                unique.insert(*feature),
                "Features in all() should be unique"
            );
        }
    }

    #[test]
    fn test_feature_from_str() {
        for feature in UnstableFeature::all() {
            let parsed = feature
                .to_string()
                .parse::<UnstableFeature>()
                .expect("Should parse from string");
            assert_eq!(*feature, parsed);
        }
    }

    #[test]
    fn test_no_features_enabled_by_default() {
        let features = UnstableFeatures::none();
        for feature in UnstableFeature::all() {
            assert!(!features.is_enabled(*feature));
        }
    }

    #[test]
    fn test_new_single() {
        let Some(&feature) = UnstableFeature::all().first() else {
            return;
        };
        let features = UnstableFeatures::new([feature]);
        assert!(features.is_enabled(feature));
    }

    #[test]
    fn test_all_features_enabled() {
        let features = UnstableFeatures::all();
        for feature in UnstableFeature::all() {
            assert!(features.is_enabled(*feature));
        }
    }

    /// A synthetic node that requires exactly one feature, for testing the
    /// check mechanism independently of any concrete syntax.
    struct Requires(UnstableFeature);

    impl RequireFeature for Requires {
        fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
            out.push(FeatureRequirement::new(self.0, Span::new(0, 3)));
        }
    }

    #[test]
    fn test_check_program_rejects_each_disabled_feature() {
        // For every feature that exists, a node requiring it
        // is rejected when no features are enabled, and the error names that
        // feature and points at the `-Z` flag.
        for &feature in UnstableFeature::all() {
            let mut handler = ErrorCollector::new();
            let source = SourceFile::anonymous(Arc::from("x"));
            UnstableFeatures::none().check_program(&Requires(feature), &source, &mut handler);

            let error = handler.to_string();
            assert!(
                error.contains(&feature.to_string()),
                "error should name the feature `{feature}`: {error}"
            );
            assert!(error.contains("not enabled"), "{error}");
            assert!(error.contains("-Z"), "{error}");
        }
    }

    #[test]
    fn test_check_program_accepts_each_enabled_feature() {
        // Enabling exactly the required feature clears the error.
        for &feature in UnstableFeature::all() {
            let mut handler = ErrorCollector::new();
            let source = SourceFile::anonymous(Arc::from("x"));
            UnstableFeatures::new([feature]).check_program(
                &Requires(feature),
                &source,
                &mut handler,
            );

            assert!(
                !handler.has_errors(),
                "enabling `{feature}` should accept a node that requires it: {}",
                handler
            );
        }
    }

    #[test]
    fn test_from_names_single() {
        let Some(&feature) = UnstableFeature::all().first() else {
            return;
        };
        let features = UnstableFeatures::from_names(vec![feature.to_string()]).unwrap();
        assert!(features.is_enabled(feature));
    }

    #[test]
    fn test_from_names_multiple() {
        let names: Vec<_> = UnstableFeature::all()
            .iter()
            .map(|f| f.to_string())
            .collect();
        let features = UnstableFeatures::from_names(names).unwrap();
        for feature in UnstableFeature::all() {
            assert!(features.is_enabled(*feature));
        }
    }

    #[test]
    fn test_from_names_empty() {
        let features = UnstableFeatures::from_names(Vec::<&str>::new()).unwrap();
        for feature in UnstableFeature::all() {
            assert!(!features.is_enabled(*feature));
        }
    }

    #[test]
    fn test_from_names_with_whitespace() {
        let Some(&feature) = UnstableFeature::all().first() else {
            return;
        };
        let features = UnstableFeatures::from_names(vec![format!("  {}  ", feature)]).unwrap();
        assert!(features.is_enabled(feature));
    }

    #[test]
    fn test_from_names_unknown() {
        let result = UnstableFeatures::from_names(vec!["unknown-feature"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown"));
    }

    #[test]
    fn test_from_names_dedups_like_new() {
        let Some(&feature) = UnstableFeature::all().first() else {
            return;
        };
        let name = feature.to_string();
        // A repeated flag (e.g. `-Z imports -Z imports`) collapses to one entry and
        // compares equal to the canonical `new` constructor — `from_names` routes
        // through `new`, so both share the sorted + deduped representation.
        let from_dupes = UnstableFeatures::from_names(vec![name.clone(), name]).unwrap();
        assert_eq!(from_dupes, UnstableFeatures::new([feature]));
    }
}
