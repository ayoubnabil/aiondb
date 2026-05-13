use super::*;

pub(super) fn lookup(name: &str) -> Option<FunctionInfo> {
    match name {
        // ── Implemented math functions ──
        "abs" => Some(FunctionInfo {
            func: ScalarFunction::Abs,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "ceil" | "ceiling" => Some(FunctionInfo {
            func: ScalarFunction::Ceil,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "floor" => Some(FunctionInfo {
            func: ScalarFunction::Floor,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "round" => Some(FunctionInfo {
            func: ScalarFunction::Round,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(2),
        }),
        "trunc" | "truncate" => Some(FunctionInfo {
            func: ScalarFunction::Trunc,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(2),
        }),
        "power" | "pow" => Some(FunctionInfo {
            func: ScalarFunction::Power,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "sqrt" => Some(FunctionInfo {
            func: ScalarFunction::Sqrt,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "log" | "log10" => Some(FunctionInfo {
            func: ScalarFunction::Log,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(2),
        }),
        "ln" => Some(FunctionInfo {
            func: ScalarFunction::Ln,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "exp" => Some(FunctionInfo {
            func: ScalarFunction::Exp,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "mod" => Some(FunctionInfo {
            func: ScalarFunction::Mod,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "sign" => Some(FunctionInfo {
            func: ScalarFunction::Sign,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "pi" => Some(FunctionInfo {
            func: ScalarFunction::Pi,
            return_type: DataType::Double,
            min_args: 0,
            max_args: Some(0),
        }),
        "random" => Some(FunctionInfo {
            func: ScalarFunction::Random,
            return_type: DataType::Double,
            min_args: 0,
            max_args: Some(0),
        }),
        "greatest" => Some(FunctionInfo {
            func: ScalarFunction::Greatest,
            return_type: DataType::Double,
            min_args: 1,
            max_args: None,
        }),
        "least" => Some(FunctionInfo {
            func: ScalarFunction::Least,
            return_type: DataType::Double,
            min_args: 1,
            max_args: None,
        }),
        // ── Implemented vector distance functions ──
        "l2_distance" => Some(FunctionInfo {
            func: ScalarFunction::L2Distance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "cosine_distance" => Some(FunctionInfo {
            func: ScalarFunction::CosineDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "inner_product" => Some(FunctionInfo {
            func: ScalarFunction::InnerProduct,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "manhattan_distance" | "l1_distance" => Some(FunctionInfo {
            func: ScalarFunction::ManhattanDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "vector_dims" => Some(FunctionInfo {
            func: ScalarFunction::VectorDims,
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "l2_norm" | "vector_norm" => Some(FunctionInfo {
            func: ScalarFunction::L2Norm,
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "l2_normalize" => Some(FunctionInfo {
            func: ScalarFunction::L2Normalize,
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 1,
            max_args: Some(1),
        }),
        "subvector" => Some(FunctionInfo {
            func: ScalarFunction::Subvector,
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 3,
            max_args: Some(3),
        }),
        "binary_quantize" => Some(FunctionInfo {
            func: ScalarFunction::BinaryQuantize,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "hamming_distance" => Some(FunctionInfo {
            func: ScalarFunction::HammingDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "jaccard_distance" => Some(FunctionInfo {
            func: ScalarFunction::JaccardDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "negative_inner_product" => Some(FunctionInfo {
            func: ScalarFunction::NegativeInnerProduct,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        // ── Implemented conversion function ──
        "to_number" => Some(FunctionInfo {
            func: ScalarFunction::ToNumber,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        // ── Stub/reserved math functions ──
        "width_bucket" => Some(FunctionInfo {
            func: ScalarFunction::Generic("width_bucket".into()),
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(4),
        }),
        "scale" | "div" | "gcd" | "lcm" | "factorial" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Numeric,
            min_args: 1,
            max_args: Some(2),
        }),
        "min_scale" | "trim_scale" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Numeric,
            min_args: 1,
            max_args: Some(1),
        }),
        // Hyperbolic / trig math functions
        "acosh" | "asinh" | "atanh" | "cosh" | "sinh" | "tanh" | "cosd" | "sind" | "tand"
        | "acosd" | "asind" | "atand" | "atan2d" | "sin" | "cos" | "tan" | "asin" | "acos"
        | "atan" | "atan2" | "cotd" | "cot" | "degrees" | "radians" | "cbrt" | "erf" | "erfc" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Double,
                min_args: 1,
                max_args: Some(2),
            })
        }
        "setseed" => Some(FunctionInfo {
            func: ScalarFunction::Generic("setseed".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "random_normal" => Some(FunctionInfo {
            func: ScalarFunction::Generic("random_normal".into()),
            return_type: DataType::Double,
            min_args: 0,
            max_args: Some(2),
        }),
        // Statistical aggregate functions
        "corr" | "covar_pop" | "covar_samp" | "regr_avgx" | "regr_avgy" | "regr_count"
        | "regr_intercept" | "regr_r2" | "regr_slope" | "regr_sxx" | "regr_sxy" | "regr_syy"
        | "stddev" | "stddev_pop" | "stddev_samp" | "variance" | "var_pop" | "var_samp" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Double,
                min_args: 1,
                max_args: Some(2),
            })
        }
        // Bit/byte manipulation
        "bit_count" => Some(FunctionInfo {
            func: ScalarFunction::Generic("bit_count".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "get_bit" | "set_bit" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(3),
        }),
        "get_byte" | "set_byte" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(3),
        }),
        // Money functions
        "cashlarger" | "cashsmaller" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Money,
            min_args: 2,
            max_args: Some(2),
        }),
        // Aggregates sometimes called as scalar (stub)
        "percentile_cont" | "percentile_disc" | "mode" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        // NULL counting functions
        "num_nulls" | "num_nonnulls" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: None,
        }),
        "__aiondb_variadic_num_nulls" | "__aiondb_variadic_num_nonnulls" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "int4mul" => Some(FunctionInfo {
            func: ScalarFunction::Generic("int4mul".into()),
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(2),
        }),
        "numeric_inc" => Some(FunctionInfo {
            func: ScalarFunction::Generic("numeric_inc".into()),
            return_type: DataType::Numeric,
            min_args: 1,
            max_args: Some(1),
        }),
        _ => None,
    }
}
