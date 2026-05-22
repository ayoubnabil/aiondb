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
        "l2_distance" | "pg_catalog.l2_distance" => Some(FunctionInfo {
            func: ScalarFunction::L2Distance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "cosine_distance" | "pg_catalog.cosine_distance" => Some(FunctionInfo {
            func: ScalarFunction::CosineDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "inner_product" | "pg_catalog.inner_product" => Some(FunctionInfo {
            func: ScalarFunction::InnerProduct,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "manhattan_distance"
        | "pg_catalog.manhattan_distance"
        | "l1_distance"
        | "pg_catalog.l1_distance" => Some(FunctionInfo {
            func: ScalarFunction::ManhattanDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "vector_dims" | "pg_catalog.vector_dims" => Some(FunctionInfo {
            func: ScalarFunction::VectorDims,
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "l2_norm" | "pg_catalog.l2_norm" | "vector_norm" | "pg_catalog.vector_norm" => {
            Some(FunctionInfo {
                func: ScalarFunction::L2Norm,
                return_type: DataType::Double,
                min_args: 1,
                max_args: Some(1),
            })
        }
        "l2_normalize" | "pg_catalog.l2_normalize" => Some(FunctionInfo {
            func: ScalarFunction::L2Normalize,
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 1,
            max_args: Some(1),
        }),
        "subvector" | "pg_catalog.subvector" => Some(FunctionInfo {
            func: ScalarFunction::Subvector,
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 3,
            max_args: Some(3),
        }),
        "binary_quantize" | "pg_catalog.binary_quantize" => Some(FunctionInfo {
            func: ScalarFunction::BinaryQuantize,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(3),
        }),
        "vector_in" | "pg_catalog.vector_in" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 1,
            max_args: Some(3),
        }),
        "halfvec_in" | "pg_catalog.halfvec_in" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float16,
            },
            min_args: 1,
            max_args: Some(3),
        }),
        "sparsevec_in" | "pg_catalog.sparsevec_in" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 1,
            max_args: Some(3),
        }),
        "vector_out"
        | "pg_catalog.vector_out"
        | "halfvec_out"
        | "pg_catalog.halfvec_out"
        | "sparsevec_out"
        | "pg_catalog.sparsevec_out" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "array_to_vector"
        | "pg_catalog.array_to_vector"
        | "halfvec_to_vector"
        | "pg_catalog.halfvec_to_vector"
        | "sparsevec_to_vector"
        | "pg_catalog.sparsevec_to_vector" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 3,
            max_args: Some(3),
        }),
        "vector_to_float4"
        | "pg_catalog.vector_to_float4"
        | "halfvec_to_float4"
        | "pg_catalog.halfvec_to_float4" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Array(Box::new(DataType::Real)),
            min_args: 3,
            max_args: Some(3),
        }),
        "array_to_halfvec"
        | "pg_catalog.array_to_halfvec"
        | "vector_to_halfvec"
        | "pg_catalog.vector_to_halfvec"
        | "sparsevec_to_halfvec"
        | "pg_catalog.sparsevec_to_halfvec" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float16,
            },
            min_args: 3,
            max_args: Some(3),
        }),
        "vector_to_sparsevec"
        | "pg_catalog.vector_to_sparsevec"
        | "halfvec_to_sparsevec"
        | "pg_catalog.halfvec_to_sparsevec"
        | "array_to_sparsevec"
        | "pg_catalog.array_to_sparsevec" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 3,
            max_args: Some(3),
        }),
        "vector_add"
        | "pg_catalog.vector_add"
        | "vector_sub"
        | "pg_catalog.vector_sub"
        | "vector_mul"
        | "pg_catalog.vector_mul"
        | "vector_concat"
        | "pg_catalog.vector_concat" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            min_args: 2,
            max_args: Some(2),
        }),
        "halfvec_add"
        | "pg_catalog.halfvec_add"
        | "halfvec_sub"
        | "pg_catalog.halfvec_sub"
        | "halfvec_mul"
        | "pg_catalog.halfvec_mul"
        | "halfvec_concat"
        | "pg_catalog.halfvec_concat" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.to_owned()),
            return_type: DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float16,
            },
            min_args: 2,
            max_args: Some(2),
        }),
        "hamming_distance" | "pg_catalog.hamming_distance" => Some(FunctionInfo {
            func: ScalarFunction::HammingDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "jaccard_distance" | "pg_catalog.jaccard_distance" => Some(FunctionInfo {
            func: ScalarFunction::JaccardDistance,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "negative_inner_product" | "pg_catalog.negative_inner_product" => Some(FunctionInfo {
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
