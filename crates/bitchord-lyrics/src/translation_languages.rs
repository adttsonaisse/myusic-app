use serde::Serialize;

/// One language the translate button can translate lyrics into.
///
/// `code` is kept verbatim as Google Translate expects it for its `tl`
/// parameter (including subtagged codes like `zh-CN`, `zh-TW`, `mni-Mtei`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationLanguage {
    pub code: &'static str,
    pub fallback_name: &'static str,
}

impl TranslationLanguage {
    pub const fn new(code: &'static str, fallback_name: &'static str) -> Self {
        Self {
            code,
            fallback_name,
        }
    }
}

/// Every language Google Translate offers, in the order its own picker lists
/// them (alphabetical by English name).
pub const TRANSLATION_LANGUAGES: &[TranslationLanguage] = &[
    TranslationLanguage::new("af", "Afrikaans"),
    TranslationLanguage::new("sq", "Albanian"),
    TranslationLanguage::new("am", "Amharic"),
    TranslationLanguage::new("ar", "Arabic"),
    TranslationLanguage::new("hy", "Armenian"),
    TranslationLanguage::new("as", "Assamese"),
    TranslationLanguage::new("ay", "Aymara"),
    TranslationLanguage::new("az", "Azerbaijani"),
    TranslationLanguage::new("bm", "Bambara"),
    TranslationLanguage::new("eu", "Basque"),
    TranslationLanguage::new("be", "Belarusian"),
    TranslationLanguage::new("bn", "Bengali"),
    TranslationLanguage::new("bho", "Bhojpuri"),
    TranslationLanguage::new("bs", "Bosnian"),
    TranslationLanguage::new("bg", "Bulgarian"),
    TranslationLanguage::new("ca", "Catalan"),
    TranslationLanguage::new("ceb", "Cebuano"),
    TranslationLanguage::new("ny", "Chichewa"),
    TranslationLanguage::new("zh-CN", "Chinese (Simplified)"),
    TranslationLanguage::new("zh-TW", "Chinese (Traditional)"),
    TranslationLanguage::new("co", "Corsican"),
    TranslationLanguage::new("hr", "Croatian"),
    TranslationLanguage::new("cs", "Czech"),
    TranslationLanguage::new("da", "Danish"),
    TranslationLanguage::new("dv", "Dhivehi"),
    TranslationLanguage::new("doi", "Dogri"),
    TranslationLanguage::new("nl", "Dutch"),
    TranslationLanguage::new("en", "English"),
    TranslationLanguage::new("eo", "Esperanto"),
    TranslationLanguage::new("et", "Estonian"),
    TranslationLanguage::new("ee", "Ewe"),
    TranslationLanguage::new("tl", "Filipino"),
    TranslationLanguage::new("fi", "Finnish"),
    TranslationLanguage::new("fr", "French"),
    TranslationLanguage::new("fy", "Frisian"),
    TranslationLanguage::new("gl", "Galician"),
    TranslationLanguage::new("ka", "Georgian"),
    TranslationLanguage::new("de", "German"),
    TranslationLanguage::new("el", "Greek"),
    TranslationLanguage::new("gn", "Guarani"),
    TranslationLanguage::new("gu", "Gujarati"),
    TranslationLanguage::new("ht", "Haitian Creole"),
    TranslationLanguage::new("ha", "Hausa"),
    TranslationLanguage::new("haw", "Hawaiian"),
    TranslationLanguage::new("iw", "Hebrew"),
    TranslationLanguage::new("hi", "Hindi"),
    TranslationLanguage::new("hmn", "Hmong"),
    TranslationLanguage::new("hu", "Hungarian"),
    TranslationLanguage::new("is", "Icelandic"),
    TranslationLanguage::new("ig", "Igbo"),
    TranslationLanguage::new("ilo", "Ilocano"),
    TranslationLanguage::new("id", "Indonesian"),
    TranslationLanguage::new("ga", "Irish"),
    TranslationLanguage::new("it", "Italian"),
    TranslationLanguage::new("ja", "Japanese"),
    TranslationLanguage::new("jw", "Javanese"),
    TranslationLanguage::new("kn", "Kannada"),
    TranslationLanguage::new("kk", "Kazakh"),
    TranslationLanguage::new("km", "Khmer"),
    TranslationLanguage::new("rw", "Kinyarwanda"),
    TranslationLanguage::new("gom", "Konkani"),
    TranslationLanguage::new("ko", "Korean"),
    TranslationLanguage::new("kri", "Krio"),
    TranslationLanguage::new("ku", "Kurdish (Kurmanji)"),
    TranslationLanguage::new("ckb", "Kurdish (Sorani)"),
    TranslationLanguage::new("ky", "Kyrgyz"),
    TranslationLanguage::new("lo", "Lao"),
    TranslationLanguage::new("la", "Latin"),
    TranslationLanguage::new("lv", "Latvian"),
    TranslationLanguage::new("ln", "Lingala"),
    TranslationLanguage::new("lt", "Lithuanian"),
    TranslationLanguage::new("lg", "Luganda"),
    TranslationLanguage::new("lb", "Luxembourgish"),
    TranslationLanguage::new("mk", "Macedonian"),
    TranslationLanguage::new("mai", "Maithili"),
    TranslationLanguage::new("mg", "Malagasy"),
    TranslationLanguage::new("ms", "Malay"),
    TranslationLanguage::new("ml", "Malayalam"),
    TranslationLanguage::new("mt", "Maltese"),
    TranslationLanguage::new("mi", "Maori"),
    TranslationLanguage::new("mr", "Marathi"),
    TranslationLanguage::new("mni-Mtei", "Meiteilon (Manipuri)"),
    TranslationLanguage::new("lus", "Mizo"),
    TranslationLanguage::new("mn", "Mongolian"),
    TranslationLanguage::new("my", "Myanmar (Burmese)"),
    TranslationLanguage::new("ne", "Nepali"),
    TranslationLanguage::new("no", "Norwegian"),
    TranslationLanguage::new("or", "Odia (Oriya)"),
    TranslationLanguage::new("om", "Oromo"),
    TranslationLanguage::new("ps", "Pashto"),
    TranslationLanguage::new("fa", "Persian"),
    TranslationLanguage::new("pl", "Polish"),
    TranslationLanguage::new("pt", "Portuguese"),
    TranslationLanguage::new("pa", "Punjabi"),
    TranslationLanguage::new("qu", "Quechua"),
    TranslationLanguage::new("ro", "Romanian"),
    TranslationLanguage::new("ru", "Russian"),
    TranslationLanguage::new("sm", "Samoan"),
    TranslationLanguage::new("sa", "Sanskrit"),
    TranslationLanguage::new("gd", "Scots Gaelic"),
    TranslationLanguage::new("nso", "Sepedi"),
    TranslationLanguage::new("sr", "Serbian"),
    TranslationLanguage::new("st", "Sesotho"),
    TranslationLanguage::new("sn", "Shona"),
    TranslationLanguage::new("sd", "Sindhi"),
    TranslationLanguage::new("si", "Sinhala"),
    TranslationLanguage::new("sk", "Slovak"),
    TranslationLanguage::new("sl", "Slovenian"),
    TranslationLanguage::new("so", "Somali"),
    TranslationLanguage::new("es", "Spanish"),
    TranslationLanguage::new("su", "Sundanese"),
    TranslationLanguage::new("sw", "Swahili"),
    TranslationLanguage::new("sv", "Swedish"),
    TranslationLanguage::new("tg", "Tajik"),
    TranslationLanguage::new("ta", "Tamil"),
    TranslationLanguage::new("tt", "Tatar"),
    TranslationLanguage::new("te", "Telugu"),
    TranslationLanguage::new("th", "Thai"),
    TranslationLanguage::new("ti", "Tigrinya"),
    TranslationLanguage::new("ts", "Tsonga"),
    TranslationLanguage::new("tr", "Turkish"),
    TranslationLanguage::new("tk", "Turkmen"),
    TranslationLanguage::new("ak", "Twi"),
    TranslationLanguage::new("uk", "Ukrainian"),
    TranslationLanguage::new("ur", "Urdu"),
    TranslationLanguage::new("ug", "Uyghur"),
    TranslationLanguage::new("uz", "Uzbek"),
    TranslationLanguage::new("vi", "Vietnamese"),
    TranslationLanguage::new("cy", "Welsh"),
    TranslationLanguage::new("xh", "Xhosa"),
    TranslationLanguage::new("yi", "Yiddish"),
    TranslationLanguage::new("yo", "Yoruba"),
    TranslationLanguage::new("zu", "Zulu"),
];

/// Looks up a language entry by code (ASCII case-insensitive).
pub fn find_translation_language(code: &str) -> Option<&'static TranslationLanguage> {
    TRANSLATION_LANGUAGES
        .iter()
        .find(|entry| entry.code.eq_ignore_ascii_case(code))
}

/// Display name for `code` using the curated table, falling back to `code`
/// itself with its first character capitalized when unrecognized.
pub fn translation_language_name(code: &str) -> String {
    translation_language_name_with_platform(code, |_| None)
}

/// Display name for `code`, preferring a platform/locale display-name lookup
/// when provided.
///
/// Subtagged codes containing `'-'` (such as `zh-CN`, `zh-TW`, `mni-Mtei`)
/// deliberately skip the platform lookup so curated script labels win over
/// territory labels like `"Chinese (China)"`.
pub fn translation_language_name_with_platform(
    code: &str,
    platform_lookup: impl FnOnce(&str) -> Option<String>,
) -> String {
    let entry = find_translation_language(code);
    let platform = if code.contains('-') {
        None
    } else {
        platform_lookup(code)
            .filter(|name| !name.trim().is_empty() && !name.eq_ignore_ascii_case(code))
    };
    let name = platform
        .as_deref()
        .or_else(|| entry.map(|e| e.fallback_name))
        .unwrap_or(code);
    capitalize_first(name)
}

fn capitalize_first(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if first.is_lowercase() {
        let mut out = String::with_capacity(name.len());
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
        out
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_all_133_google_translate_languages() {
        assert_eq!(133, TRANSLATION_LANGUAGES.len());
        assert_eq!("af", TRANSLATION_LANGUAGES[0].code);
        assert_eq!("zu", TRANSLATION_LANGUAGES[132].code);
    }

    #[test]
    fn resolves_known_subtagged_and_unknown_language_codes() {
        assert_eq!("Japanese", translation_language_name("ja"));
        assert_eq!("Chinese (Simplified)", translation_language_name("zh-cn"));
        assert_eq!("Chinese (Traditional)", translation_language_name("ZH-TW"));
        assert_eq!(
            "Meiteilon (Manipuri)",
            translation_language_name("mni-Mtei")
        );
        assert_eq!("Xyz", translation_language_name("xyz"));
    }

    #[test]
    fn platform_lookup_overrides_bare_codes_but_not_subtagged_or_echoed_codes() {
        assert_eq!(
            "Japonés",
            translation_language_name_with_platform("ja", |_| Some("japonés".to_owned()))
        );
        // Subtagged codes skip platform lookup to preserve script names.
        assert_eq!(
            "Chinese (Simplified)",
            translation_language_name_with_platform("zh-CN", |_| Some(
                "Chinese (China)".to_owned()
            ))
        );
        // Echoed unknown platform name falls back to the table.
        assert_eq!(
            "Bhojpuri",
            translation_language_name_with_platform("bho", |c| Some(c.to_owned()))
        );
    }
}
