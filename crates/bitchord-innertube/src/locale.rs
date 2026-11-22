/// Script-preserving locale tag conversion for YouTube Music (`hl`/`gl`) and
/// Google Translate (`tl`).
pub struct LocaleTags;

impl LocaleTags {
    pub const ZH_HANT: &'static str = "zh-Hant";
    pub const ZH_HANS: &'static str = "zh-Hans";
    pub const ZH_TW_WIRE: &'static str = "zh-TW";
    pub const ZH_CN_WIRE: &'static str = "zh-CN";

    /// Normalizes any incoming tag (`"zh_TW"`, `"zh-HK"`, `"zh"`, `"en_US"`, ...)
    /// to `"zh-Hant"` / `"zh-Hans"` / base ISO-639 language.
    pub fn normalize_app_tag(raw: Option<&str>) -> String {
        let Some(trimmed) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return "en".to_owned();
        };
        let tag = trimmed.replace('_', "-");
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("zh") {
            if lower.contains("hant") {
                return Self::ZH_HANT.to_owned();
            }
            if lower.contains("hans") {
                return Self::ZH_HANS.to_owned();
            }
            if lower.contains("tw") || lower.contains("hk") || lower.contains("mo") {
                return Self::ZH_HANT.to_owned();
            }
            if lower.contains("cn") || lower.contains("sg") || lower.contains("my") {
                return Self::ZH_HANS.to_owned();
            }
            return Self::ZH_HANS.to_owned();
        }
        let base = lower.split('-').next().unwrap_or_default();
        match base {
            "iw" => "he".to_owned(),
            "in" => "id".to_owned(),
            "ji" => "yi".to_owned(),
            "" => "en".to_owned(),
            other => other.to_owned(),
        }
    }

    /// Reads the process locale (`LC_ALL` / `LC_MESSAGES` / `LANG`), falling
    /// back to `"en"`.
    pub fn system_tag() -> String {
        let env_val = std::env::var("LC_ALL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("LANG").ok().filter(|s| !s.is_empty()));
        let cleaned = env_val.as_deref().map(|raw| {
            raw.split('.')
                .next()
                .unwrap_or(raw)
                .split('@')
                .next()
                .unwrap_or(raw)
        });
        match cleaned {
            Some("C") | Some("POSIX") | None => "en".to_owned(),
            other => Self::normalize_app_tag(other),
        }
    }

    /// Value YouTube Music expects for `hl`.
    pub fn yt_hl(app_tag: &str) -> String {
        let norm = Self::normalize_app_tag(Some(app_tag));
        match norm.as_str() {
            Self::ZH_HANT => Self::ZH_TW_WIRE.to_owned(),
            Self::ZH_HANS => Self::ZH_CN_WIRE.to_owned(),
            _ => norm.split('-').next().unwrap_or("en").to_owned(),
        }
    }

    /// Geo code for InnerTube `context.client.gl` on browse/stats requests.
    pub fn yt_gl(app_tag: &str) -> &'static str {
        match Self::normalize_app_tag(Some(app_tag)).as_str() {
            Self::ZH_HANT => "TW",
            Self::ZH_HANS => "SG",
            _ => "US",
        }
    }

    /// Geo code for search requests (`None` for non-Chinese locales so YouTube
    /// ranks results by connection geolocation).
    pub fn yt_search_gl(app_tag: &str) -> Option<&'static str> {
        match Self::normalize_app_tag(Some(app_tag)).as_str() {
            Self::ZH_HANT => Some("TW"),
            Self::ZH_HANS => Some("SG"),
            _ => None,
        }
    }

    pub fn accept_language(app_tag: &str) -> String {
        let norm = Self::normalize_app_tag(Some(app_tag));
        match norm.as_str() {
            Self::ZH_HANT => "zh-TW,zh-Hant;q=0.9,zh;q=0.8,en-US;q=0.7,en;q=0.6".to_owned(),
            Self::ZH_HANS => "zh-CN,zh-Hans;q=0.9,zh;q=0.8,en-US;q=0.7,en;q=0.6".to_owned(),
            "en" => "en-US,en;q=0.9".to_owned(),
            _ => {
                let base = norm.split('-').next().unwrap_or("en");
                format!("{base},en-US;q=0.8,en;q=0.7")
            }
        }
    }

    pub fn translation_wire_target(tag: &str) -> String {
        let t = tag.trim().replace('_', "-");
        if t.eq_ignore_ascii_case("zh-TW") || t.eq_ignore_ascii_case("zh-Hant") {
            return Self::ZH_TW_WIRE.to_owned();
        }
        if t.eq_ignore_ascii_case("zh-CN") || t.eq_ignore_ascii_case("zh-Hans") {
            return Self::ZH_CN_WIRE.to_owned();
        }
        match Self::normalize_app_tag(Some(&t)).as_str() {
            Self::ZH_HANT => Self::ZH_TW_WIRE.to_owned(),
            Self::ZH_HANS => Self::ZH_CN_WIRE.to_owned(),
            _ => {
                if t.is_empty() {
                    "en".to_owned()
                } else {
                    t
                }
            }
        }
    }

    /// Script-aware equality: `zh-Hant != zh-Hans != zh`.
    pub fn same_language_script_aware(first: &str, second: &str) -> bool {
        fn canon(tag: &str) -> String {
            let lower = tag.trim().replace('_', "-").to_ascii_lowercase();
            if lower.starts_with("zh") {
                if lower.contains("tw")
                    || lower.contains("hant")
                    || lower.contains("hk")
                    || lower.contains("mo")
                {
                    return "zh-hant".to_owned();
                }
                if lower.contains("cn") || lower.contains("hans") || lower.contains("sg") {
                    return "zh-hans".to_owned();
                }
                return "zh".to_owned();
            }
            lower.split('-').next().unwrap_or_default().to_owned()
        }
        canon(first) == canon(second)
    }

    pub fn is_chinese_tag(tag: &str) -> bool {
        tag.trim()
            .replace('_', "-")
            .to_ascii_lowercase()
            .starts_with("zh")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_chinese_scripts_and_legacy_tags() {
        assert_eq!(LocaleTags::normalize_app_tag(Some("zh_TW")), "zh-Hant");
        assert_eq!(LocaleTags::normalize_app_tag(Some("zh-HK")), "zh-Hant");
        assert_eq!(LocaleTags::normalize_app_tag(Some("zh")), "zh-Hans");
        assert_eq!(LocaleTags::normalize_app_tag(Some("in_ID")), "id");
        assert_eq!(LocaleTags::normalize_app_tag(Some("iw-IL")), "he");
        assert_eq!(LocaleTags::yt_hl("zh-Hant"), "zh-TW");
        assert_eq!(LocaleTags::yt_gl("zh-Hant"), "TW");
        assert_eq!(LocaleTags::yt_search_gl("zh-Hans"), Some("SG"));
        assert_eq!(LocaleTags::yt_search_gl("id"), None);
        assert!(!LocaleTags::same_language_script_aware(
            "zh-Hant", "zh-Hans"
        ));
        assert!(LocaleTags::same_language_script_aware("zh-TW", "zh-Hant"));
    }
}
