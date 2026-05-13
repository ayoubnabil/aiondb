use std::fmt;

/// Element type for vector columns.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum VectorElementType {
    /// 32-bit IEEE 754 float (default, 4 bytes per dimension).
    #[default]
    Float32,
    /// 16-bit IEEE 754 half-precision float (2 bytes per dimension).
    Float16,
    /// Unsigned 8-bit integer (1 byte per dimension).
    Uint8,
}

impl VectorElementType {
    /// Bytes consumed per vector dimension for this element type.
    #[must_use]
    pub const fn bytes_per_dim(self) -> usize {
        match self {
            Self::Float32 => 4,
            Self::Float16 => 2,
            Self::Uint8 => 1,
        }
    }

    /// Parse from a string (case-insensitive).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "float32" | "f32" | "real" => Some(Self::Float32),
            "float16" | "f16" | "half" => Some(Self::Float16),
            "uint8" | "u8" | "byte" => Some(Self::Uint8),
            _ => None,
        }
    }

    /// PostgreSQL-style type name suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Float32 => "float32",
            Self::Float16 => "float16",
            Self::Uint8 => "uint8",
        }
    }
}

impl fmt::Display for VectorElementType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TextTypeModifier {
    Char { length: u32 },
    VarChar { length: u32 },
    BpChar,
    VarCharAny,
    Name,
    InternalChar,
    Oid,
    Int2Vector,
    OidVector,
    RegProc,
    RegProcedure,
    RegOper,
    RegOperator,
    RegClass,
    RegType,
    RegConfig,
    RegDictionary,
    RegNamespace,
    RegRole,
    RegCollation,
}

impl TextTypeModifier {
    #[must_use]
    pub const fn length(self) -> u32 {
        match self {
            Self::Char { length } | Self::VarChar { length } => length,
            Self::BpChar
            | Self::VarCharAny
            | Self::Name
            | Self::InternalChar
            | Self::Oid
            | Self::Int2Vector
            | Self::OidVector
            | Self::RegProc
            | Self::RegProcedure
            | Self::RegOper
            | Self::RegOperator
            | Self::RegClass
            | Self::RegType
            | Self::RegConfig
            | Self::RegDictionary
            | Self::RegNamespace
            | Self::RegRole
            | Self::RegCollation => 0,
        }
    }

    #[must_use]
    pub fn atttypmod(self) -> i32 {
        if matches!(
            self,
            Self::BpChar
                | Self::VarCharAny
                | Self::Name
                | Self::InternalChar
                | Self::Oid
                | Self::Int2Vector
                | Self::OidVector
                | Self::RegProc
                | Self::RegProcedure
                | Self::RegOper
                | Self::RegOperator
                | Self::RegClass
                | Self::RegType
                | Self::RegConfig
                | Self::RegDictionary
                | Self::RegNamespace
                | Self::RegRole
                | Self::RegCollation
        ) {
            return -1;
        }
        let Some(with_header) = self.length().checked_add(4) else {
            return i32::MAX;
        };
        i32::try_from(with_header).unwrap_or(i32::MAX)
    }

    #[must_use]
    pub const fn scalar_type_oid(self) -> u32 {
        match self {
            Self::Char { .. } | Self::BpChar => 1042,
            Self::VarChar { .. } | Self::VarCharAny => 1043,
            Self::Name => 19,
            Self::InternalChar => 18,
            Self::Oid => 26,
            Self::Int2Vector => 22,
            Self::OidVector => 30,
            Self::RegProc => 24,
            Self::RegProcedure => 2202,
            Self::RegOper => 2203,
            Self::RegOperator => 2204,
            Self::RegClass => 2205,
            Self::RegType => 2206,
            Self::RegConfig => 3734,
            Self::RegDictionary => 3769,
            Self::RegNamespace => 4089,
            Self::RegRole => 4096,
            Self::RegCollation => 4191,
        }
    }

    #[must_use]
    pub const fn array_type_oid(self) -> u32 {
        match self {
            Self::Char { .. } | Self::BpChar => 1014,
            Self::VarChar { .. } | Self::VarCharAny => 1015,
            Self::Name => 1003,
            Self::InternalChar => 1002,
            Self::Oid | Self::OidVector => 1028,
            Self::Int2Vector => 1005,
            Self::RegProc => 1008,
            Self::RegProcedure => 2207,
            Self::RegOper => 2208,
            Self::RegOperator => 2209,
            Self::RegClass => 2210,
            Self::RegType => 2211,
            Self::RegConfig => 3735,
            Self::RegDictionary => 3770,
            Self::RegNamespace => 4090,
            Self::RegRole => 4097,
            Self::RegCollation => 4192,
        }
    }

    #[must_use]
    pub fn pg_display_name(self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Char { length } => std::borrow::Cow::Owned(format!("character({length})")),
            Self::VarChar { length } => {
                std::borrow::Cow::Owned(format!("character varying({length})"))
            }
            Self::BpChar => std::borrow::Cow::Borrowed("character"),
            Self::VarCharAny => std::borrow::Cow::Borrowed("character varying"),
            Self::Name => std::borrow::Cow::Borrowed("name"),
            Self::InternalChar => std::borrow::Cow::Borrowed("\"char\""),
            Self::Oid => std::borrow::Cow::Borrowed("oid"),
            Self::Int2Vector => std::borrow::Cow::Borrowed("int2vector"),
            Self::OidVector => std::borrow::Cow::Borrowed("oidvector"),
            Self::RegProc => std::borrow::Cow::Borrowed("regproc"),
            Self::RegProcedure => std::borrow::Cow::Borrowed("regprocedure"),
            Self::RegOper => std::borrow::Cow::Borrowed("regoper"),
            Self::RegOperator => std::borrow::Cow::Borrowed("regoperator"),
            Self::RegClass => std::borrow::Cow::Borrowed("regclass"),
            Self::RegType => std::borrow::Cow::Borrowed("regtype"),
            Self::RegConfig => std::borrow::Cow::Borrowed("regconfig"),
            Self::RegDictionary => std::borrow::Cow::Borrowed("regdictionary"),
            Self::RegNamespace => std::borrow::Cow::Borrowed("regnamespace"),
            Self::RegRole => std::borrow::Cow::Borrowed("regrole"),
            Self::RegCollation => std::borrow::Cow::Borrowed("regcollation"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DataType {
    Int,
    BigInt,
    Real,
    Double,
    Numeric,
    Money,
    Text,
    Boolean,
    Blob,
    Timestamp,
    Date,
    Time,
    TimeTz,
    Interval,
    Tid,
    Uuid,
    TimestampTz,
    PgLsn,
    Jsonb,
    MacAddr,
    MacAddr8,
    Vector {
        dims: u32,
        #[serde(default)]
        element_type: VectorElementType,
    },
    Array(Box<DataType>),
}

impl DataType {
    /// Returns the PostgreSQL-canonical lowercase type name used in error messages.
    #[must_use]
    pub const fn pg_type_name(&self) -> &'static str {
        match self {
            Self::Int => "integer",
            Self::BigInt => "bigint",
            Self::Real => "real",
            Self::Double => "double precision",
            Self::Numeric => "numeric",
            Self::Money => "money",
            Self::Text => "text",
            Self::Boolean => "boolean",
            Self::Blob => "bytea",
            Self::Timestamp => "timestamp without time zone",
            Self::Date => "date",
            Self::Time => "time without time zone",
            Self::TimeTz => "time with time zone",
            Self::Interval => "interval",
            Self::Tid => "tid",
            Self::Uuid => "uuid",
            Self::TimestampTz => "timestamp with time zone",
            Self::PgLsn => "pg_lsn",
            Self::Jsonb => "jsonb",
            Self::MacAddr => "macaddr",
            Self::MacAddr8 => "macaddr8",
            Self::Vector { .. } => "vector",
            Self::Array(_) => "array",
        }
    }

    #[must_use]
    pub const fn pg_oid(&self) -> Option<u32> {
        match self {
            Self::Int => Some(23),
            Self::BigInt => Some(20),
            Self::Real => Some(700),
            Self::Double => Some(701),
            Self::Numeric => Some(1700),
            Self::Money => Some(790),
            Self::Text => Some(25),
            Self::Boolean => Some(16),
            Self::Blob => Some(17),
            Self::Timestamp => Some(1114),
            Self::Date => Some(1082),
            Self::Time => Some(1083),
            Self::TimeTz => Some(1266),
            Self::Interval => Some(1186),
            Self::Tid => Some(27),
            Self::Uuid => Some(2950),
            Self::TimestampTz => Some(1184),
            Self::PgLsn => Some(3220),
            Self::Jsonb => Some(3802),
            Self::MacAddr => Some(829),
            Self::MacAddr8 => Some(774),
            Self::Vector { .. } => None,
            Self::Array(inner) => match inner.pg_oid() {
                Some(23) => Some(1007),   // INT[]
                Some(20) => Some(1016),   // BIGINT[]
                Some(700) => Some(1021),  // REAL[]
                Some(701) => Some(1022),  // DOUBLE[]
                Some(1700) => Some(1231), // NUMERIC[]
                Some(790) => Some(791),   // MONEY[]
                Some(25) => Some(1009),   // TEXT[]
                Some(27) => Some(1010),   // TID[]
                Some(16) => Some(1000),   // BOOLEAN[]
                Some(17) => Some(1001),   // BLOB/BYTEA[]
                Some(1114) => Some(1115), // TIMESTAMP[]
                Some(1082) => Some(1182), // DATE[]
                Some(1083) => Some(1183), // TIME[]
                Some(1266) => Some(1270), // TIMETZ[]
                Some(1186) => Some(1187), // INTERVAL[]
                Some(2950) => Some(2951), // UUID[]
                Some(1184) => Some(1185), // TIMESTAMPTZ[]
                Some(3220) => Some(3221), // PG_LSN[]
                Some(3802) => Some(3807), // JSONB[]
                Some(829) => Some(1040),  // MACADDR[]
                _ => None,
            },
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int => f.write_str("INT"),
            Self::BigInt => f.write_str("BIGINT"),
            Self::Real => f.write_str("REAL"),
            Self::Double => f.write_str("DOUBLE"),
            Self::Numeric => f.write_str("NUMERIC"),
            Self::Money => f.write_str("MONEY"),
            Self::Text => f.write_str("TEXT"),
            Self::Boolean => f.write_str("BOOLEAN"),
            Self::Blob => f.write_str("BLOB"),
            Self::Timestamp => f.write_str("TIMESTAMP"),
            Self::Date => f.write_str("DATE"),
            Self::Time => f.write_str("TIME"),
            Self::TimeTz => f.write_str("TIMETZ"),
            Self::Interval => f.write_str("INTERVAL"),
            Self::Tid => f.write_str("TID"),
            Self::Uuid => f.write_str("UUID"),
            Self::TimestampTz => f.write_str("TIMESTAMPTZ"),
            Self::PgLsn => f.write_str("PG_LSN"),
            Self::Jsonb => f.write_str("JSONB"),
            Self::MacAddr => f.write_str("MACADDR"),
            Self::MacAddr8 => f.write_str("MACADDR8"),
            Self::Vector { dims, element_type } => {
                if *dims == 0 && *element_type == VectorElementType::Float32 {
                    f.write_str("VECTOR")
                } else if *element_type == VectorElementType::Float32 {
                    write!(f, "VECTOR({dims})")
                } else {
                    write!(f, "VECTOR({dims}, {element_type})")
                }
            }
            Self::Array(inner) => write!(f, "{inner}[]"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---------------------------------------------------------------
    // pg_oid() returns correct OID for EVERY variant
    // ---------------------------------------------------------------

    #[test]
    fn pg_oid_int() {
        assert_eq!(DataType::Int.pg_oid(), Some(23));
    }

    #[test]
    fn pg_oid_bigint() {
        assert_eq!(DataType::BigInt.pg_oid(), Some(20));
    }

    #[test]
    fn pg_oid_real() {
        assert_eq!(DataType::Real.pg_oid(), Some(700));
    }

    #[test]
    fn pg_oid_double() {
        assert_eq!(DataType::Double.pg_oid(), Some(701));
    }

    #[test]
    fn pg_oid_numeric() {
        assert_eq!(DataType::Numeric.pg_oid(), Some(1700));
    }

    #[test]
    fn pg_oid_text() {
        assert_eq!(DataType::Text.pg_oid(), Some(25));
    }

    #[test]
    fn pg_oid_boolean() {
        assert_eq!(DataType::Boolean.pg_oid(), Some(16));
    }

    #[test]
    fn pg_oid_blob() {
        assert_eq!(DataType::Blob.pg_oid(), Some(17));
    }

    #[test]
    fn pg_oid_timestamp() {
        assert_eq!(DataType::Timestamp.pg_oid(), Some(1114));
    }

    #[test]
    fn pg_oid_date() {
        assert_eq!(DataType::Date.pg_oid(), Some(1082));
    }

    #[test]
    fn pg_oid_interval() {
        assert_eq!(DataType::Interval.pg_oid(), Some(1186));
    }

    #[test]
    fn pg_oid_uuid() {
        assert_eq!(DataType::Uuid.pg_oid(), Some(2950));
    }

    #[test]
    fn pg_oid_timestamptz() {
        assert_eq!(DataType::TimestampTz.pg_oid(), Some(1184));
    }

    #[test]
    fn pg_oid_tid() {
        assert_eq!(DataType::Tid.pg_oid(), Some(27));
    }

    #[test]
    fn pg_oid_array_tid() {
        assert_eq!(
            DataType::Array(Box::new(DataType::Tid)).pg_oid(),
            Some(1010)
        );
    }

    #[test]
    fn pg_oid_array_int() {
        assert_eq!(
            DataType::Array(Box::new(DataType::Int)).pg_oid(),
            Some(1007)
        );
    }

    #[test]
    fn pg_oid_array_text() {
        assert_eq!(
            DataType::Array(Box::new(DataType::Text)).pg_oid(),
            Some(1009)
        );
    }

    #[test]
    fn pg_oid_vector_returns_none() {
        assert_eq!(
            DataType::Vector {
                dims: 3,
                element_type: crate::VectorElementType::Float32
            }
            .pg_oid(),
            None
        );
    }

    #[test]
    fn pg_oid_vector_zero_dims_returns_none() {
        assert_eq!(
            DataType::Vector {
                dims: 0,
                element_type: crate::VectorElementType::Float32
            }
            .pg_oid(),
            None
        );
    }

    #[test]
    fn pg_oid_vector_max_dims_returns_none() {
        assert_eq!(
            DataType::Vector {
                dims: u32::MAX,
                element_type: crate::VectorElementType::Float32
            }
            .pg_oid(),
            None
        );
    }

    // ---------------------------------------------------------------
    // Display for EVERY variant
    // ---------------------------------------------------------------

    #[test]
    fn display_int() {
        assert_eq!(DataType::Int.to_string(), "INT");
    }

    #[test]
    fn display_bigint() {
        assert_eq!(DataType::BigInt.to_string(), "BIGINT");
    }

    #[test]
    fn display_real() {
        assert_eq!(DataType::Real.to_string(), "REAL");
    }

    #[test]
    fn display_double() {
        assert_eq!(DataType::Double.to_string(), "DOUBLE");
    }

    #[test]
    fn display_numeric() {
        assert_eq!(DataType::Numeric.to_string(), "NUMERIC");
    }

    #[test]
    fn display_text() {
        assert_eq!(DataType::Text.to_string(), "TEXT");
    }

    #[test]
    fn display_boolean() {
        assert_eq!(DataType::Boolean.to_string(), "BOOLEAN");
    }

    #[test]
    fn display_blob() {
        assert_eq!(DataType::Blob.to_string(), "BLOB");
    }

    #[test]
    fn display_timestamp() {
        assert_eq!(DataType::Timestamp.to_string(), "TIMESTAMP");
    }

    #[test]
    fn display_date() {
        assert_eq!(DataType::Date.to_string(), "DATE");
    }

    #[test]
    fn display_interval() {
        assert_eq!(DataType::Interval.to_string(), "INTERVAL");
    }

    #[test]
    fn display_uuid() {
        assert_eq!(DataType::Uuid.to_string(), "UUID");
    }

    #[test]
    fn display_timestamptz() {
        assert_eq!(DataType::TimestampTz.to_string(), "TIMESTAMPTZ");
    }

    #[test]
    fn display_tid() {
        assert_eq!(DataType::Tid.to_string(), "TID");
    }

    #[test]
    fn display_array_int() {
        assert_eq!(
            DataType::Array(Box::new(DataType::Int)).to_string(),
            "INT[]"
        );
    }

    #[test]
    fn display_array_text() {
        assert_eq!(
            DataType::Array(Box::new(DataType::Text)).to_string(),
            "TEXT[]"
        );
    }

    #[test]
    fn display_vector_dims_3() {
        assert_eq!(
            DataType::Vector {
                dims: 3,
                element_type: crate::VectorElementType::Float32
            }
            .to_string(),
            "VECTOR(3)"
        );
    }

    #[test]
    fn display_vector_dims_0() {
        assert_eq!(
            DataType::Vector {
                dims: 0,
                element_type: crate::VectorElementType::Float32
            }
            .to_string(),
            "VECTOR"
        );
    }

    #[test]
    fn display_vector_dims_max() {
        assert_eq!(
            DataType::Vector {
                dims: u32::MAX,
                element_type: crate::VectorElementType::Float32
            }
            .to_string(),
            format!("VECTOR({})", u32::MAX)
        );
    }

    // ---------------------------------------------------------------
    // Clone, Eq, Hash
    // ---------------------------------------------------------------

    #[test]
    fn clone_preserves_equality_for_all_variants() {
        let variants: Vec<DataType> = vec![
            DataType::Int,
            DataType::BigInt,
            DataType::Real,
            DataType::Double,
            DataType::Numeric,
            DataType::Text,
            DataType::Boolean,
            DataType::Blob,
            DataType::Timestamp,
            DataType::Date,
            DataType::Interval,
            DataType::Uuid,
            DataType::TimestampTz,
            DataType::Tid,
            DataType::Jsonb,
            DataType::Vector {
                dims: 128,
                element_type: crate::VectorElementType::Float32,
            },
            DataType::Array(Box::new(DataType::Int)),
        ];
        for v in &variants {
            assert_eq!(v, &v.clone());
        }
    }

    #[test]
    fn eq_same_variant_is_equal() {
        assert_eq!(DataType::Int, DataType::Int);
        assert_eq!(DataType::Text, DataType::Text);
    }

    #[test]
    fn eq_different_variants_not_equal() {
        assert_ne!(DataType::Int, DataType::BigInt);
        assert_ne!(DataType::Real, DataType::Double);
        assert_ne!(DataType::Text, DataType::Blob);
        assert_ne!(DataType::Timestamp, DataType::Date);
    }

    #[test]
    fn vector_same_dims_are_equal() {
        assert_eq!(
            DataType::Vector {
                dims: 42,
                element_type: crate::VectorElementType::Float32
            },
            DataType::Vector {
                dims: 42,
                element_type: crate::VectorElementType::Float32
            }
        );
    }

    #[test]
    fn vector_different_dims_are_not_equal() {
        assert_ne!(
            DataType::Vector {
                dims: 3,
                element_type: crate::VectorElementType::Float32
            },
            DataType::Vector {
                dims: 4,
                element_type: crate::VectorElementType::Float32
            }
        );
    }

    #[test]
    fn hash_same_value_consistent() {
        let mut set = HashSet::new();
        set.insert(DataType::Int);
        set.insert(DataType::Int);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn hash_different_values_distinct() {
        let mut set = HashSet::new();
        set.insert(DataType::Int);
        set.insert(DataType::BigInt);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn hash_vector_same_dims_consistent() {
        let mut set = HashSet::new();
        set.insert(DataType::Vector {
            dims: 10,
            element_type: crate::VectorElementType::Float32,
        });
        set.insert(DataType::Vector {
            dims: 10,
            element_type: crate::VectorElementType::Float32,
        });
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn hash_vector_different_dims_distinct() {
        let mut set = HashSet::new();
        set.insert(DataType::Vector {
            dims: 10,
            element_type: crate::VectorElementType::Float32,
        });
        set.insert(DataType::Vector {
            dims: 20,
            element_type: crate::VectorElementType::Float32,
        });
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn hash_all_non_vector_variants_distinct() {
        let mut set = HashSet::new();
        set.insert(DataType::Int);
        set.insert(DataType::BigInt);
        set.insert(DataType::Real);
        set.insert(DataType::Double);
        set.insert(DataType::Numeric);
        set.insert(DataType::Text);
        set.insert(DataType::Boolean);
        set.insert(DataType::Blob);
        set.insert(DataType::Timestamp);
        set.insert(DataType::Date);
        set.insert(DataType::Interval);
        set.insert(DataType::Uuid);
        set.insert(DataType::TimestampTz);
        set.insert(DataType::Tid);
        set.insert(DataType::Jsonb);
        set.insert(DataType::Array(Box::new(DataType::Int)));
        assert_eq!(set.len(), 16);
    }

    // ---------------------------------------------------------------
    // NEW: pg_oid() uniqueness - all non-None OIDs are distinct
    // ---------------------------------------------------------------

    #[test]
    fn pg_oid_all_values_are_unique() {
        let oids: Vec<Option<u32>> = vec![
            DataType::Int.pg_oid(),
            DataType::BigInt.pg_oid(),
            DataType::Real.pg_oid(),
            DataType::Double.pg_oid(),
            DataType::Numeric.pg_oid(),
            DataType::Text.pg_oid(),
            DataType::Boolean.pg_oid(),
            DataType::Blob.pg_oid(),
            DataType::Timestamp.pg_oid(),
            DataType::Date.pg_oid(),
            DataType::Interval.pg_oid(),
            DataType::Uuid.pg_oid(),
            DataType::TimestampTz.pg_oid(),
            DataType::Tid.pg_oid(),
            DataType::Jsonb.pg_oid(),
        ];
        let non_none: Vec<u32> = oids.into_iter().flatten().collect();
        let unique: HashSet<u32> = non_none.iter().copied().collect();
        assert_eq!(non_none.len(), unique.len());
    }

    // ---------------------------------------------------------------
    // NEW: Display produces non-empty strings for all
    // ---------------------------------------------------------------

    #[test]
    fn display_all_non_vector_are_nonempty() {
        let variants: Vec<DataType> = vec![
            DataType::Int,
            DataType::BigInt,
            DataType::Real,
            DataType::Double,
            DataType::Numeric,
            DataType::Text,
            DataType::Boolean,
            DataType::Blob,
            DataType::Timestamp,
            DataType::Date,
            DataType::Interval,
            DataType::Uuid,
            DataType::TimestampTz,
            DataType::Tid,
            DataType::Jsonb,
        ];
        for v in &variants {
            let s = v.to_string();
            assert!(!s.is_empty(), "Display for {v:?} should not be empty");
        }
    }

    #[test]
    fn display_all_uppercase() {
        let variants: Vec<DataType> = vec![
            DataType::Int,
            DataType::BigInt,
            DataType::Real,
            DataType::Double,
            DataType::Numeric,
            DataType::Text,
            DataType::Boolean,
            DataType::Blob,
            DataType::Timestamp,
            DataType::Date,
            DataType::Interval,
            DataType::Uuid,
            DataType::TimestampTz,
            DataType::Tid,
            DataType::Jsonb,
        ];
        for v in &variants {
            let s = v.to_string();
            assert_eq!(s, s.to_uppercase(), "Display for {v:?} should be uppercase");
        }
    }

    // ---------------------------------------------------------------
    // NEW: Vector Display with various dims
    // ---------------------------------------------------------------

    #[test]
    fn display_vector_dims_1() {
        assert_eq!(
            DataType::Vector {
                dims: 1,
                element_type: crate::VectorElementType::Float32
            }
            .to_string(),
            "VECTOR(1)"
        );
    }

    #[test]
    fn display_vector_dims_128() {
        assert_eq!(
            DataType::Vector {
                dims: 128,
                element_type: crate::VectorElementType::Float32
            }
            .to_string(),
            "VECTOR(128)"
        );
    }

    #[test]
    fn display_vector_dims_1024() {
        assert_eq!(
            DataType::Vector {
                dims: 1024,
                element_type: crate::VectorElementType::Float32
            }
            .to_string(),
            "VECTOR(1024)"
        );
    }

    // ---------------------------------------------------------------
    // NEW: Debug trait for DataType
    // ---------------------------------------------------------------

    #[test]
    fn debug_int() {
        let dbg = format!("{:?}", DataType::Int);
        assert!(dbg.contains("Int"));
    }

    #[test]
    fn debug_vector() {
        let dbg = format!(
            "{:?}",
            DataType::Vector {
                dims: 42,
                element_type: crate::VectorElementType::Float32
            }
        );
        assert!(dbg.contains("Vector"));
        assert!(dbg.contains("42"));
    }

    // ---------------------------------------------------------------
    // NEW: Clone for vector with specific dims
    // ---------------------------------------------------------------

    #[test]
    fn clone_vector_preserves_dims() {
        let v = DataType::Vector {
            dims: 999,
            element_type: crate::VectorElementType::Float32,
        };
        let c = v.clone();
        assert_eq!(v, c);
        if let DataType::Vector { dims, .. } = c {
            assert_eq!(dims, 999);
        } else {
            panic!("expected Vector");
        }
    }

    // ---------------------------------------------------------------
    // NEW: Hash with vectors of various dims
    // ---------------------------------------------------------------

    #[test]
    fn hash_vector_three_different_dims_distinct() {
        let mut set = HashSet::new();
        set.insert(DataType::Vector {
            dims: 1,
            element_type: crate::VectorElementType::Float32,
        });
        set.insert(DataType::Vector {
            dims: 2,
            element_type: crate::VectorElementType::Float32,
        });
        set.insert(DataType::Vector {
            dims: 3,
            element_type: crate::VectorElementType::Float32,
        });
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn hash_includes_vectors_and_non_vectors() {
        let mut set = HashSet::new();
        set.insert(DataType::Int);
        set.insert(DataType::Vector {
            dims: 0,
            element_type: crate::VectorElementType::Float32,
        });
        set.insert(DataType::Vector {
            dims: 1,
            element_type: crate::VectorElementType::Float32,
        });
        assert_eq!(set.len(), 3);
    }

    // ---------------------------------------------------------------
    // NEW: Eq between Vector and non-Vector
    // ---------------------------------------------------------------

    #[test]
    fn vector_not_equal_to_int() {
        assert_ne!(
            DataType::Vector {
                dims: 0,
                element_type: crate::VectorElementType::Float32
            },
            DataType::Int
        );
    }

    #[test]
    fn vector_not_equal_to_blob() {
        assert_ne!(
            DataType::Vector {
                dims: 0,
                element_type: crate::VectorElementType::Float32
            },
            DataType::Blob
        );
    }

    #[test]
    fn vector_not_equal_to_text() {
        assert_ne!(
            DataType::Vector {
                dims: 0,
                element_type: crate::VectorElementType::Float32
            },
            DataType::Text
        );
    }

    // ---------------------------------------------------------------
    // NEW: pg_oid for Vector is always None regardless of dims
    // ---------------------------------------------------------------

    #[test]
    fn pg_oid_vector_dims_1_returns_none() {
        assert_eq!(
            DataType::Vector {
                dims: 1,
                element_type: crate::VectorElementType::Float32
            }
            .pg_oid(),
            None
        );
    }

    #[test]
    fn pg_oid_vector_dims_1024_returns_none() {
        assert_eq!(
            DataType::Vector {
                dims: 1024,
                element_type: crate::VectorElementType::Float32
            }
            .pg_oid(),
            None
        );
    }
}
