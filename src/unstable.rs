use std::fmt;
use std::str::FromStr;

use crate::error::{Error, ErrorCollector, RichError, Span};
use crate::source::SourceFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnstableFeature {
    /// Import and module syntax, including `use`, `crate::`, and import aliases.
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
            _ => Err(format!("Unknown unstable feature: '{}'", s)),
        }
    }
}

impl UnstableFeature {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Imports => "Import syntax: 'use', 'crate::', and import aliases",
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

        let mut help = String::from("Enable unstable features. Available features:\n");
        for feature in Self::all() {
            help.push_str(&format!(
                "  {name:<width$} {desc}\n",
                name = feature.to_string(),
                width = max_len + 2,
                desc = feature.description()
            ));
        }
        help
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
/// the check. Derive the impl with `#[derive(RequireFeature)]`: it recurses into
/// every field, so a field whose type has no impl is a compile error until the
/// feature requirements of that type are decided. Nodes that own feature-gated
/// syntax declare it with `#[require_feature(requires(FeatureName))]`. Only two
/// kinds of impl are written by hand: atoms that can never carry gated syntax
/// (see [`impl_require_feature_never`]) and boundary types whose traversal needs
/// custom logic (see `AliasedType` in `types.rs`).
pub(crate) trait RequireFeature {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>);
}

/// Derive macro generating an exhaustive [`RequireFeature`] impl.
///
/// See the trait documentation and `simplicityhl-derive` for details.
pub(crate) use simplicityhl_derive::RequireFeature;

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

impl<T: RequireFeature> RequireFeature for Vec<T> {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        self.as_slice().feature_requirements(out);
    }
}

impl<T: RequireFeature> RequireFeature for Option<T> {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        if let Some(inner) = self {
            inner.feature_requirements(out);
        }
    }
}

impl<A: RequireFeature, B: RequireFeature> RequireFeature for (A, B) {
    fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
        self.0.feature_requirements(out);
        self.1.feature_requirements(out);
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

/// Implement [`RequireFeature`] as a no-op for atoms that can never contain
/// feature-gated syntax.
///
/// Listing a type here is a per-type promise that no part of its grammar will
/// ever be gated — the same promise `#[require_feature(skip)]` makes per field,
/// but recorded once in a single greppable place. If a type outgrows the
/// promise (its syntax gains a feature-gated form), remove it from this list
/// and write an exhaustive impl by hand, like the one for `AliasedType` in
/// `types.rs`.
macro_rules! impl_require_feature_never {
    ($($ty:ty),* $(,)?) => {
        $(
            impl RequireFeature for $ty {
                fn feature_requirements(&self, _out: &mut Vec<FeatureRequirement>) {}
            }
        )*
    };
}

impl_require_feature_never!(
    bool,
    usize,
    std::num::NonZeroUsize,
    crate::error::Span,
    crate::num::NonZeroPow2Usize,
    crate::str::AliasName,
    crate::str::Binary,
    crate::str::Decimal,
    crate::str::FunctionName,
    crate::str::Hexadecimal,
    crate::str::Identifier,
    crate::str::JetName,
    crate::str::ModuleName,
    crate::str::SymbolName,
    crate::str::WitnessName,
);

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
        Self {
            enabled_features: features.into_iter().collect(),
        }
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
                    feature_name: req.feature.to_string(),
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
        Ok(Self {
            enabled_features: features,
        })
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
    fn test_all_features() {
        let all_features = UnstableFeature::all();
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

    #[test]
    fn test_check_program_disabled() {
        struct RequiresImports;

        impl RequireFeature for RequiresImports {
            fn feature_requirements(&self, out: &mut Vec<FeatureRequirement>) {
                out.push(FeatureRequirement::new(
                    UnstableFeature::Imports,
                    Span::new(0, 3),
                ));
            }
        }

        let mut handler = ErrorCollector::new();
        let source = SourceFile::anonymous(Arc::from("use"));
        UnstableFeatures::none().check_program(&RequiresImports, &source, &mut handler);

        let error = handler.to_string();
        assert!(error.contains("imports"));
        assert!(error.contains("not enabled"));
        assert!(error.contains("-Z"));
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
}
