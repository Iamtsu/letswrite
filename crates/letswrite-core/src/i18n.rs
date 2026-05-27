//! Internationalization: loads Fluent bundles and resolves user-facing strings.
//!
//! Translations live under `crates/letswrite-core/i18n/<lang>/*.ftl`. The
//! English bundle is compiled in via `include_str!` as the always-available
//! fallback; other languages are loaded from disk at runtime via [`I18n::load_extra`].
//!
//! Lookup order on `tr(key)`:
//! 1. The bundle for the requested language (negotiated against available bundles).
//! 2. The English bundle (always present).
//! 3. The key itself, surrounded by `‹ ›` to flag missing translations in the UI.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use fluent::{FluentArgs, FluentBundle, FluentResource};
use fluent_langneg::{negotiate_languages, NegotiationStrategy};
use unic_langid::LanguageIdentifier;

use crate::error::{Error, Result};

const EN_FTL: &str = include_str!("../i18n/en/letswrite.ftl");

/// In-memory registry of Fluent bundles, keyed by language.
pub struct I18n {
    bundles: HashMap<LanguageIdentifier, FluentBundle<FluentResource>>,
    current: LanguageIdentifier,
    fallback: LanguageIdentifier,
}

impl std::fmt::Debug for I18n {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I18n")
            .field("current", &self.current.to_string())
            .field("fallback", &self.fallback.to_string())
            .field("available_languages", &self.bundles.keys().map(ToString::to_string).collect::<Vec<_>>())
            .finish()
    }
}

impl I18n {
    /// Build a registry seeded with the compiled-in English bundle, with
    /// `current` negotiated against available languages.
    pub fn with_language(current: LanguageIdentifier) -> Result<Self> {
        let fallback: LanguageIdentifier =
            "en".parse().expect("\"en\" is a valid language tag");

        let mut bundles = HashMap::new();
        bundles.insert(fallback.clone(), build_bundle(&fallback, EN_FTL)?);

        let mut me = Self { bundles, current: fallback.clone(), fallback };
        me.select(current);
        Ok(me)
    }

    /// Load every `<lang>/letswrite.ftl` file under `dir` into the registry.
    /// Missing dir is not an error (returns 0).
    pub fn load_extra(&mut self, dir: &Path) -> Result<usize> {
        if !dir.exists() {
            return Ok(0);
        }
        let mut loaded = 0;
        for entry in fs::read_dir(dir).map_err(|e| Error::io_at(dir, e))? {
            let entry = entry.map_err(|e| Error::io_at(dir, e))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(lang_str) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let lang: LanguageIdentifier = match lang_str.parse() {
                Ok(l) => l,
                Err(_) => continue,
            };
            let ftl_path = path.join("letswrite.ftl");
            if !ftl_path.exists() {
                continue;
            }
            let source =
                fs::read_to_string(&ftl_path).map_err(|e| Error::io_at(&ftl_path, e))?;
            let bundle = build_bundle(&lang, &source)?;
            self.bundles.insert(lang, bundle);
            loaded += 1;
        }
        Ok(loaded)
    }

    /// Switch the active language. The choice is negotiated against the set
    /// of loaded bundles, so `pt-BR` falls back to `pt` if only `pt` is
    /// available, and to English if neither is.
    pub fn select(&mut self, lang: LanguageIdentifier) {
        let available: Vec<LanguageIdentifier> = self.bundles.keys().cloned().collect();
        let chosen = negotiate_languages(
            &[lang],
            &available,
            Some(&self.fallback),
            NegotiationStrategy::Filtering,
        );
        self.current = chosen.first().map_or_else(|| self.fallback.clone(), |l| (*l).clone());
    }

    pub const fn current(&self) -> &LanguageIdentifier {
        &self.current
    }

    /// Resolve a string with no arguments.
    pub fn tr(&self, key: &str) -> String {
        self.tr_args(key, None)
    }

    /// Resolve a string with Fluent arguments (for interpolation).
    pub fn tr_args(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        if let Some(s) = self.format_in(&self.current, key, args) {
            return s;
        }
        if self.current != self.fallback {
            if let Some(s) = self.format_in(&self.fallback, key, args) {
                return s;
            }
        }
        format!("‹{key}›")
    }

    fn format_in(
        &self,
        lang: &LanguageIdentifier,
        key: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Option<String> {
        let bundle = self.bundles.get(lang)?;
        let msg = bundle.get_message(key)?;
        let pattern = msg.value()?;
        let mut errors = Vec::new();
        let s = bundle.format_pattern(pattern, args, &mut errors).into_owned();
        if !errors.is_empty() {
            tracing::warn!(?errors, key, language = %lang, "fluent format errors");
        }
        Some(s)
    }
}

fn build_bundle(
    lang: &LanguageIdentifier,
    source: &str,
) -> Result<FluentBundle<FluentResource>> {
    let resource = FluentResource::try_new(source.to_owned())
        .map_err(|(_, errs)| Error::InvalidData(format!("ftl parse errors: {errs:?}")))?;
    let mut bundle = FluentBundle::new(vec![lang.clone()]);
    // Suppress Unicode bidi isolation marks — they make English output look
    // garbled in terminal logs. Re-enable if/when we serve RTL languages.
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .map_err(|errs| Error::InvalidData(format!("ftl bundle errors: {errs:?}")))?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_fallback_returns_known_key() {
        let en: LanguageIdentifier = "en".parse().unwrap();
        let i18n = I18n::with_language(en).unwrap();
        assert_eq!(i18n.tr("app-title"), "letswrite");
    }

    #[test]
    fn unknown_key_is_marked() {
        let en: LanguageIdentifier = "en".parse().unwrap();
        let i18n = I18n::with_language(en).unwrap();
        assert_eq!(i18n.tr("does-not-exist"), "‹does-not-exist›");
    }

    #[test]
    fn requesting_unloaded_language_falls_back_to_english() {
        let de: LanguageIdentifier = "de".parse().unwrap();
        let i18n = I18n::with_language(de).unwrap();
        assert_eq!(i18n.current().to_string(), "en");
        assert_eq!(i18n.tr("app-title"), "letswrite");
    }

    #[test]
    fn load_extra_picks_up_extra_language() {
        let tmp = tempfile::tempdir().unwrap();
        let de_dir = tmp.path().join("de");
        fs::create_dir_all(&de_dir).unwrap();
        fs::write(de_dir.join("letswrite.ftl"), "app-title = lasunsschreiben\n").unwrap();

        let de: LanguageIdentifier = "de".parse().unwrap();
        let mut i18n = I18n::with_language(de.clone()).unwrap();
        let loaded = i18n.load_extra(tmp.path()).unwrap();
        assert_eq!(loaded, 1);

        i18n.select(de);
        assert_eq!(i18n.tr("app-title"), "lasunsschreiben");
    }
}
