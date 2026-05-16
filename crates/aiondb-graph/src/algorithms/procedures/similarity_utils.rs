use super::AlgorithmConfig;

pub(super) fn metric_from_config(
    procedure: &str,
    config: &AlgorithmConfig,
    default: crate::algorithms::similarity::SimilarityMetric,
) -> Result<crate::algorithms::similarity::SimilarityMetric, String> {
    let Some(metric) = config.metric.as_deref() else {
        return Ok(default);
    };
    match metric {
        value if value.eq_ignore_ascii_case("jaccard") => {
            Ok(crate::algorithms::similarity::SimilarityMetric::Jaccard)
        }
        value if value.eq_ignore_ascii_case("overlap") => {
            Ok(crate::algorithms::similarity::SimilarityMetric::Overlap)
        }
        value
            if value.eq_ignore_ascii_case("adamic_adar")
                || value.eq_ignore_ascii_case("adamicAdar") =>
        {
            Ok(crate::algorithms::similarity::SimilarityMetric::AdamicAdar)
        }
        _ => Err(format!(
            "{procedure} metric must be one of: jaccard, overlap, adamic_adar"
        )),
    }
}
