use std::fmt;

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TidValue {
    block: u32,
    offset: u16,
}

impl TidValue {
    #[must_use]
    pub const fn new(block: u32, offset: u16) -> Self {
        Self { block, offset }
    }

    #[must_use]
    pub const fn block(self) -> u32 {
        self.block
    }

    #[must_use]
    pub const fn offset(self) -> u16 {
        self.offset
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        let inner = trimmed.strip_prefix('(')?.strip_suffix(')')?;
        let (block, offset) = inner.split_once(',')?;
        let block = block.trim().parse::<u32>().ok()?;
        let offset = offset.trim().parse::<u16>().ok()?;
        Some(Self::new(block, offset))
    }
}

impl fmt::Display for TidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({},{})", self.block, self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::TidValue;

    #[test]
    fn parse_tid_with_spaces() {
        let tid = TidValue::parse("(12, 34)").expect("valid tid");
        assert_eq!(tid, TidValue::new(12, 34));
        assert_eq!(tid.to_string(), "(12,34)");
    }

    #[test]
    fn parse_tid_rejects_invalid_text() {
        assert!(TidValue::parse("12,34").is_none());
        assert!(TidValue::parse("(x,1)").is_none());
        assert!(TidValue::parse("(1,70000)").is_none());
    }
}
