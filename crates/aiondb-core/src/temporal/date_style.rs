use crate::COMPAT_DATE_STYLE;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DateOrder {
    Mdy,
    Dmy,
    Ymd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DateStyleFamily {
    Postgres,
    Iso,
    Sql,
    German,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DateStyleSetting {
    family: DateStyleFamily,
    order: DateOrder,
}

impl Default for DateStyleSetting {
    fn default() -> Self {
        Self::parse(COMPAT_DATE_STYLE)
    }
}

impl DateStyleSetting {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        Self::try_parse_with_base(raw, Self::default_base()).unwrap_or_else(Self::default_base)
    }

    #[must_use]
    pub fn try_parse(raw: &str) -> Option<Self> {
        Self::try_parse_with_base(raw, Self::default_base())
    }

    #[must_use]
    pub fn parse_with_base(raw: &str, base: Self) -> Self {
        Self::try_parse_with_base(raw, base).unwrap_or(base)
    }

    #[must_use]
    pub fn try_parse_with_base(raw: &str, base: Self) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
            return Some(Self::default());
        }

        let mut family = base.family;
        let mut order = base.order;
        let mut saw_order = false;
        let mut saw_german = false;
        let mut saw_token = false;

        for token in trimmed
            .split([',', ' '])
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            saw_token = true;
            match token.to_ascii_lowercase().as_str() {
                "postgres" => family = DateStyleFamily::Postgres,
                "iso" => family = DateStyleFamily::Iso,
                "sql" => family = DateStyleFamily::Sql,
                "german" => {
                    family = DateStyleFamily::German;
                    saw_german = true;
                }
                "us" | "mdy" => {
                    order = DateOrder::Mdy;
                    saw_order = true;
                }
                "european" | "nonus" | "dmy" => {
                    order = DateOrder::Dmy;
                    saw_order = true;
                }
                "ymd" => {
                    order = DateOrder::Ymd;
                    saw_order = true;
                }
                _ => return None,
            }
        }

        if !saw_token {
            return None;
        }

        if saw_german && !saw_order {
            order = DateOrder::Dmy;
        }

        Some(Self { family, order })
    }

    #[must_use]
    pub const fn family(self) -> DateStyleFamily {
        self.family
    }

    #[must_use]
    pub const fn order(self) -> DateOrder {
        self.order
    }

    #[must_use]
    pub fn show_value(self) -> String {
        format!("{}, {}", self.family_label(), self.order_label())
    }

    fn family_label(self) -> &'static str {
        match self.family {
            DateStyleFamily::Postgres => "Postgres",
            DateStyleFamily::Iso => "ISO",
            DateStyleFamily::Sql => "SQL",
            DateStyleFamily::German => "German",
        }
    }

    fn order_label(self) -> &'static str {
        match self.order {
            DateOrder::Mdy => "MDY",
            DateOrder::Dmy => "DMY",
            DateOrder::Ymd => "YMD",
        }
    }

    const fn default_base() -> Self {
        Self {
            family: DateStyleFamily::Iso,
            order: DateOrder::Mdy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn german_implies_dmy_order() {
        let style = DateStyleSetting::parse("German");
        assert_eq!(style.family(), DateStyleFamily::German);
        assert_eq!(style.order(), DateOrder::Dmy);
    }

    #[test]
    fn canonicalizes_show_output() {
        let style = DateStyleSetting::parse("European,Postgres");
        assert_eq!(style.show_value(), "Postgres, DMY");
    }

    #[test]
    fn preserves_base_family_for_order_only_updates() {
        let base = DateStyleSetting::parse("Postgres, MDY");
        let style = DateStyleSetting::parse_with_base("dmy", base);
        assert_eq!(style.show_value(), "Postgres, DMY");
    }

    #[test]
    fn rejects_unknown_datestyle_tokens() {
        assert_eq!(DateStyleSetting::try_parse("garbage, mdy"), None);
    }
}
