//! The catalogues, and choosing between them.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use scour_core::Catalog;

/// Every language shipped, as (BCP-47 tag, endonym): someone looking for their
/// own language recognises "Türkçe" and may not recognise "Turkish".
pub const LANGUAGES: &[(&str, &str)] = &[("en", "English"), ("tr", "Türkçe")];

/// Compiled in: a search tool that cannot find its own translation files is a
/// worse bug than an untranslated string.
const TR: &str = include_str!("../../../lang/tr/LC_MESSAGES/scour.po");

fn table(tag: &str) -> Option<&'static HashMap<String, String>> {
    static TR_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    match tag {
        "tr" => Some(TR_MAP.get_or_init(|| crate::po::parse(TR))),
        _ => None,
    }
}

/// One language's strings.
#[derive(Debug, Clone)]
pub struct Catalogue {
    tag: &'static str,
    map: Option<&'static HashMap<String, String>>,
}

impl Catalogue {
    /// The catalogue for a BCP-47 tag, falling back to English. Only the primary
    /// subtag is consulted, so `tr-TR` and `tr-CY` get the same strings.
    pub fn for_language(tag: &str) -> Catalogue {
        let primary = tag
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match LANGUAGES.iter().find(|(t, _)| *t == primary) {
            Some((t, _)) => Catalogue {
                tag: t,
                map: table(t),
            },
            None => Catalogue::english(),
        }
    }

    /// The catalogue the environment asks for.
    pub fn from_environment() -> Catalogue {
        Catalogue::for_language(&system_language())
    }

    pub const fn english() -> Catalogue {
        Catalogue {
            tag: "en",
            map: None,
        }
    }

    /// The language this catalogue speaks, as a primary subtag: `tr`, `en`. A face
    /// punctuates numbers in the language of the text around them — `5.356.281`
    /// against `5,356,281` — and only the catalogue knows which is being spoken.
    pub fn language(&self) -> &'static str {
        self.tag
    }

    /// Is this language actually translated, or only accepted?
    pub fn is_translated(&self) -> bool {
        self.map.is_some_and(|m| !m.is_empty())
    }

    /// Is this string in the catalogue, or is [`Catalog::get`] about to fall back
    /// to the English? Presence and not difference: a correct translation is
    /// sometimes identical to its msgid, and that is not a gap.
    pub fn has(&self, msgid: &str) -> bool {
        self.map.is_some_and(|m| m.contains_key(msgid))
    }

    /// How many strings this catalogue carries.
    pub fn len(&self) -> usize {
        self.map.map_or(0, |m| m.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every msgid and its translation, for the face that cannot link this crate:
    /// a few kilobytes handed over once instead of a request per label. Empty for
    /// English, where the msgid is already the answer.
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.map
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Which language to speak — the one place the order lives, most explicit first:
/// `chosen` ([`scour_settings::Settings::language`]), `configured` (`ui.language`),
/// [`system_language`], English. An empty string is skipped, not treated as one.
pub fn choose(chosen: &str, configured: &str) -> String {
    for candidate in [chosen, configured] {
        let candidate = candidate.trim();
        if !candidate.is_empty() {
            return candidate.to_owned();
        }
    }
    system_language()
}

impl Default for Catalogue {
    fn default() -> Self {
        Catalogue::from_environment()
    }
}

impl Catalog for Catalogue {
    fn locale(&self) -> &str {
        self.tag
    }

    fn get<'a>(&'a self, msgid: &'a str) -> Cow<'a, str> {
        match self.map.and_then(|m| m.get(msgid)) {
            Some(s) => Cow::Borrowed(s.as_str()),
            // The English source is the key, so a missing entry is still correct.
            None => Cow::Borrowed(msgid),
        }
    }
}

/// The language the environment asks for: `SCOUR_LANG` first, so one program can
/// be switched alone, then `LC_ALL`, `LC_MESSAGES`, `LANG` in POSIX order.
pub fn system_language() -> String {
    for key in ["SCOUR_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim();
            if v.is_empty() || v == "C" || v == "POSIX" {
                continue;
            }
            // `tr_TR.UTF-8` — take the language, drop the encoding.
            return v.split('.').next().unwrap_or(v).to_owned();
        }
    }
    #[cfg(windows)]
    {
        // No environment variable on Windows; English until a face asks the system.
        return "en".into();
    }
    #[cfg(not(windows))]
    "en".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_is_shipped_and_populated() {
        let c = Catalogue::for_language("tr");
        assert_eq!(c.locale(), "tr");
        assert!(
            c.is_translated(),
            "the Turkish catalogue should not be empty"
        );
        assert_eq!(c.get("Folder"), "Klasör");
    }

    #[test]
    fn a_region_variant_gets_the_language() {
        assert_eq!(Catalogue::for_language("tr_TR.UTF-8").locale(), "tr");
        assert_eq!(Catalogue::for_language("tr-CY").locale(), "tr");
        assert_eq!(Catalogue::for_language("TR").locale(), "tr");
    }

    #[test]
    fn an_unknown_language_degrades_to_english_rather_than_to_keys() {
        let c = Catalogue::for_language("kl");
        assert_eq!(c.locale(), "en");
        assert_eq!(c.get("Folder"), "Folder", "the English source is the key");
        assert!(!c.is_translated());
    }

    #[test]
    fn a_missing_string_degrades_to_english_too() {
        let c = Catalogue::for_language("tr");
        let unknown = "Something nobody has translated yet";
        assert_eq!(c.get(unknown), unknown);
    }

    #[test]
    fn presence_is_a_question_get_cannot_answer() {
        let c = Catalogue::for_language("tr");
        assert!(c.has("Folder"));
        // The two `get` cannot tell apart: both come back as themselves.
        assert!(!c.has("Something nobody has translated yet"));
        assert!(
            c.has("Video"),
            "translated, and identical to its msgid — which is why coverage \
             asks whether the entry is there and not whether it differs"
        );
        // English has no catalogue: the msgid is the string.
        assert!(!Catalogue::english().has("Folder"));
    }

    #[test]
    fn every_shipped_language_can_be_loaded() {
        for (tag, name) in LANGUAGES {
            let c = Catalogue::for_language(tag);
            assert_eq!(c.locale(), *tag, "{tag}");
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn a_catalogue_can_be_handed_over_whole() {
        let c = Catalogue::for_language("tr");
        let all: HashMap<_, _> = c.entries().collect();
        assert_eq!(all.len(), c.len());
        assert_eq!(all.get("Folder").copied(), Some("Klasör"));
        // English has nothing to hand over: the msgid is already the string.
        assert_eq!(Catalogue::english().entries().count(), 0);
    }

    /// The two steps of [`choose`] that do not touch the environment; the third is
    /// in the environment test below, kept to one test because it is threaded.
    #[test]
    fn an_explicit_choice_outranks_a_config_file() {
        assert_eq!(choose("en", "tr"), "en", "the person's own choice");
        assert_eq!(choose("", "tr"), "tr", "then the config file");
        // Whitespace is not a choice, and neither is "".
        assert_eq!(choose("  ", "tr"), "tr");
        assert_eq!(choose("tr", ""), "tr");
    }

    #[test]
    fn the_environment_is_read_in_posix_order() {
        // Serialised by running in one test: these are process-wide.
        let saved: Vec<_> = ["SCOUR_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
            .map(|k| (k, std::env::var(k).ok()))
            .to_vec();
        for (k, _) in &saved {
            unsafe { std::env::remove_var(k) };
        }

        unsafe { std::env::set_var("LANG", "tr_TR.UTF-8") };
        assert_eq!(system_language(), "tr_TR");
        unsafe { std::env::set_var("LC_ALL", "en_GB.UTF-8") };
        assert_eq!(system_language(), "en_GB", "LC_ALL wins");
        unsafe { std::env::set_var("SCOUR_LANG", "tr") };
        assert_eq!(system_language(), "tr", "one program can be switched alone");

        // `choose`'s last step, here because these variables are process-wide.
        assert_eq!(choose("", ""), "tr", "nothing chosen: the environment");
        assert_eq!(choose("en", ""), "en", "a choice still outranks it");

        // The POSIX "no locale" values are not a language.
        for (k, _) in &saved {
            unsafe { std::env::remove_var(k) };
        }
        unsafe { std::env::set_var("LANG", "C") };
        assert_eq!(system_language(), "en");

        for (k, v) in saved {
            match v {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}
