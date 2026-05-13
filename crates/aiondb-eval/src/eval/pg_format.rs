mod apply;
mod apply_helpers;
mod compile;
mod error;
mod model;

pub(crate) type CompiledFormat = model::CompiledFormat;
pub(crate) type FormatError = error::FormatError;
pub(crate) type ParsedFields = model::ParsedFields;

#[inline]
pub(crate) fn compile_pg_format(fmt: &str) -> CompiledFormat {
    compile::compile_pg_format(fmt)
}

#[inline]
pub(crate) fn apply_format(
    input: &str,
    format: &CompiledFormat,
) -> Result<ParsedFields, FormatError> {
    apply::apply_format(input, format)
}

#[inline]
pub(crate) fn build_date(fields: &ParsedFields, input: &str) -> Result<time::Date, FormatError> {
    apply::build_date(fields, input)
}

#[inline]
pub(crate) fn build_timestamp_components(
    fields: &ParsedFields,
    input: &str,
) -> Result<(time::PrimitiveDateTime, Option<time::UtcOffset>), FormatError> {
    apply::build_timestamp_components(fields, input)
}
