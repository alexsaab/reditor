use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Ru,
    En,
    De,
    Es,
}

impl Language {
    pub const ALL: [Self; 4] = [Self::Ru, Self::En, Self::De, Self::Es];
    pub fn label(self) -> &'static str {
        match self {
            Self::Ru => "Русский",
            Self::En => "English",
            Self::De => "Deutsch",
            Self::Es => "Español",
        }
    }
}

pub struct I18n {
    pub language: Language,
    strings: HashMap<String, String>,
}
impl I18n {
    pub fn new(language: Language) -> Self {
        let source = match language {
            Language::Ru => include_str!("../locales/ru.json"),
            Language::En => include_str!("../locales/en.json"),
            Language::De => include_str!("../locales/de.json"),
            Language::Es => include_str!("../locales/es.json"),
        };
        Self {
            language,
            strings: serde_json::from_str(source).expect("bundled translations"),
        }
    }
    pub fn t(&self, key: &str) -> &str {
        self.strings.get(key).map(String::as_str).unwrap_or("?")
    }
    pub fn code(&self) -> &str {
        match self.language {
            Language::Ru => "RU",
            Language::En => "EN",
            Language::De => "DE",
            Language::Es => "ES",
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translations_have_same_keys() {
        let reference = I18n::new(Language::En);
        for language in [Language::Ru, Language::De, Language::Es] {
            let other = I18n::new(language);
            assert_eq!(reference.strings.len(), other.strings.len());
            for key in reference.strings.keys() {
                assert!(
                    other.strings.get(key).is_some_and(|s| !s.is_empty()),
                    "{language:?}: {key}"
                );
            }
        }
    }
}
