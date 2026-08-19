//! The catalogues, and choosing between them.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use scour_core::Catalog;

/// Every language shipped, as (BCP-47 tag, name in that language).
///
/// The endonym rather than the English name: someone looking for their own
/// language recognises "Türkçe" and may not recognise "Turkish".
pub const LANGUAGES: &[(&str, &str)] = &[("en", "English"), ("tr", "Türkçe")];

/// Catalogues are compiled in. A search tool that cannot find its own
/// translation files is a worse bug than an untranslated string, and neither
/// Windows nor Android has a gettext runtime worth relying on.
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
    /// The catalogue for a BCP-47 tag, falling back to English.
    ///
    /// Only the primary subtag is consulted: `tr-TR` and `tr-CY` get the same
    /// strings, which is right until someone writes a catalogue that says
    /// otherwise.
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

    /// Is this language actually translated, or only accepted?
    /// The language this catalogue speaks, as a primary subtag: `tr`, `en`.
    ///
    /// **A window punctuates numbers in the language of the text around them**
    /// — `5.356.281` in Turkish, `5,356,281` in English — and the catalogue is
    /// the only thing that knows which one is being spoken. Asking the desktop
    /// instead was wrong on the case that matters: a Turkish desktop showing
    /// an English window.
    pub fn language(&self) -> &'static str {
        self.tag
    }

    pub fn is_translated(&self) -> bool {
        self.map.is_some_and(|m| !m.is_empty())
    }

    /// Is this string in the catalogue, or is [`Catalog::get`] about to fall
    /// back to the English?
    ///
    /// `get` cannot answer that, and should not: it hands back the msgid when
    /// nothing is translated, which is the right answer for a caller showing
    /// text and a useless one for a caller checking coverage. A test that asked
    /// `!get(id).is_empty()` was true for every string in every language,
    /// translated or not, and so could never fail.
    ///
    /// Presence rather than difference, because a correct translation is
    /// sometimes identical to its msgid: Turkish spells `Video` the way English
    /// does, and a check for a *different* string would call that a gap.
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

    /// Every msgid and its translation.
    ///
    /// For the one frontend that cannot link this crate. A browser cannot call
    /// [`Catalog::get`] per string, and asking a bridge per string would be one
    /// request per label; the whole Turkish catalogue is a few kilobytes, so it
    /// is handed over once and looked up in the page with the same rule this
    /// implements — present means translated, absent means the msgid is already
    /// the answer.
    ///
    /// Empty for English, which is not an oversight: English has no catalogue
    /// because the msgid *is* the English, and a page that receives nothing
    /// falls back to the msgids it already has written into it.
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.map
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Which language to speak, given what the person chose and what the machine
/// says — the one place the order lives.
///
/// Most explicit first:
///
/// 1. `chosen` — [`scour_settings::Settings::language`], written by whichever
///    frontend last offered a menu. A person who picked English in the window
///    meant it in the terminal too.
/// 2. `configured` — `config.toml`'s `ui.language`, for a machine that wants
///    one answer without a menu having been opened.
/// 3. The environment, via [`system_language`]. POSIX order, and the answer
///    every other program on the machine gives.
/// 4. English.
///
/// **Written down once because it was on its way to being written down three
/// times.** `scour-gui` had a private copy that read `LC_ALL`/`LC_MESSAGES`/
/// `LANG` itself and disagreed with this crate in two ways — it did not consult
/// `SCOUR_LANG`, so the one variable that exists to switch a single program did
/// nothing for the window, and it did not skip `C`/`POSIX`, so a session with
/// `LANG=C` asked for a language called "C". A third frontend would have
/// written a third.
///
/// Empty strings are skipped rather than treated as a choice: "" is what
/// `Settings::language` holds before anybody decides, and what `ui.language`
/// ships as.
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
            // The English source *is* the key, so a missing entry is still a
            // correct answer — just an untranslated one.
            None => Cow::Borrowed(msgid),
        }
    }
}

/// The language the environment asks for.
///
/// `LC_ALL`, then `LC_MESSAGES`, then `LANG` — the order POSIX specifies —
/// and `SCOUR_LANG` before any of them, so one program can be switched without
/// changing the whole session. Reading these directly rather than through a
/// crate keeps this dependency-free; the values are simple and the rules are
/// short.
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
        // No environment variable on Windows; the user interface will ask the
        // system properly. Until then, English.
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
        // English has nothing to hand over, and that is the correct amount:
        // the msgid is already the string.
        assert_eq!(Catalogue::english().entries().count(), 0);
    }

    /// The two steps of [`choose`] that do not touch the environment. The
    /// third is asserted inside the environment test below, which is one test
    /// on purpose — these variables are process-wide and the test runner is
    /// threaded.
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

        // `choose`'s last step, here rather than in its own test because these
        // variables belong to the process and the runner is threaded.
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
