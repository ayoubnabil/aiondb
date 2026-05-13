use aiondb_core::{DbError, DbResult, ErrorReport, NumericValue, SqlState};

#[derive(Clone, Copy, Debug)]
pub(crate) struct GeomPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GeomBounds {
    pub(crate) xmin: f64,
    pub(crate) xmax: f64,
    pub(crate) ymin: f64,
    pub(crate) ymax: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GeomCircle {
    pub(crate) center: GeomPoint,
    pub(crate) radius: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct GeomPath {
    pub(crate) closed: bool,
    pub(crate) points: Vec<GeomPoint>,
}

pub(crate) fn validate_geometric_literal(type_name: &str, input: &str) -> DbResult<()> {
    match type_name {
        "point" => parse_point_text(input).map(|_| ()),
        "box" => parse_box_text(input).map(|_| ()),
        "line" => parse_line_text(input),
        "lseg" => parse_lseg_text(input).map(|_| ()),
        "path" => parse_path_text(input).map(|_| ()),
        "polygon" => parse_polygon_text(input).map(|_| ()),
        "circle" => parse_circle_text(input).map(|_| ()),
        _ => Ok(()),
    }
}

pub(crate) fn parse_point_text(input: &str) -> DbResult<GeomPoint> {
    let trimmed = input.trim();
    let inner = if let Some(inner) = trimmed.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        inner.trim()
    } else {
        trimmed
    };
    let (x, y) = parse_two_numbers(inner, "point", input)?;
    Ok(GeomPoint { x, y })
}

pub(crate) fn parse_circle_text(input: &str) -> DbResult<GeomCircle> {
    let mut text = input.trim();
    if text.is_empty() {
        return Err(invalid_geom_input("circle", input));
    }

    if starts_with_ascii_case_insensitive(text, "circle(") {
        let open = text
            .find('(')
            .ok_or_else(|| invalid_geom_input("circle", input))?;
        let close =
            find_matching_paren(text, open).ok_or_else(|| invalid_geom_input("circle", input))?;
        if close + 1 != text.len() {
            return Err(invalid_geom_input("circle", input));
        }
        text = text[open + 1..close].trim();
    }

    if let Some(inner) = text.strip_prefix('<') {
        text = inner
            .strip_suffix('>')
            .map(str::trim)
            .ok_or_else(|| invalid_geom_input("circle", input))?;
    }

    if let Some(inner) = text.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        let inner = inner.trim();
        if inner.starts_with('(') {
            text = inner;
        }
    }

    if text.starts_with('(') || starts_with_ascii_case_insensitive(text, "point(") {
        if let Ok((center, rest)) = parse_prefixed_point(text, "circle", input) {
            let rest = rest.trim_start();
            let radius_text = rest
                .strip_prefix(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid_geom_input("circle", input))?;
            if radius_text.contains(',') {
                return Err(invalid_geom_input("circle", input));
            }
            let radius = parse_geom_number(radius_text, "circle", input)?;
            if radius.is_finite() && radius < 0.0 {
                return Err(invalid_geom_input("circle", input));
            }
            return Ok(GeomCircle { center, radius });
        }
    }

    let numbers = parse_number_list(text, 3, "circle", input)?;
    let radius = numbers[2];
    if radius.is_finite() && radius < 0.0 {
        return Err(invalid_geom_input("circle", input));
    }
    Ok(GeomCircle {
        center: GeomPoint {
            x: numbers[0],
            y: numbers[1],
        },
        radius,
    })
}

pub(crate) fn parse_box_text(input: &str) -> DbResult<(GeomPoint, GeomPoint)> {
    let mut text = input.trim();
    if starts_with_ascii_case_insensitive(text, "box(") {
        let open = text
            .find('(')
            .ok_or_else(|| invalid_geom_input("box", input))?;
        let close =
            find_matching_paren(text, open).ok_or_else(|| invalid_geom_input("box", input))?;
        if close + 1 != text.len() {
            return Err(invalid_geom_input("box", input));
        }
        text = text[open + 1..close].trim();
    }

    if let Ok(numbers) = parse_number_list(text, 4, "box", input) {
        return Ok((
            GeomPoint {
                x: numbers[0],
                y: numbers[1],
            },
            GeomPoint {
                x: numbers[2],
                y: numbers[3],
            },
        ));
    }

    let text = strip_optional_outer_point_pair(text);
    if let Ok((first, rest)) = parse_prefixed_point(text, "box", input) {
        let rest = rest.trim_start();
        if !rest.is_empty() {
            // PostgreSQL accepts both "(x1,y1),(x2,y2)" and "(x1,y1)(x2,y2)".
            let rest = rest.strip_prefix(',').map(str::trim_start).unwrap_or(rest);
            if let Ok((second, tail)) = parse_prefixed_point(rest, "box", input) {
                if tail.trim().is_empty() {
                    return Ok((first, second));
                }
            }
        }
    }

    Err(invalid_geom_input("box", input))
}

pub(crate) fn parse_path_text(input: &str) -> DbResult<GeomPath> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(invalid_geom_input("path", input));
    }

    let (closed, inner) = if let Some(inner) = trimmed.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or_else(|| invalid_geom_input("path", input))?;
        (false, inner.trim())
    } else if trimmed.starts_with('(') && trimmed.ends_with(')') {
        let inner = trimmed
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .ok_or_else(|| invalid_geom_input("path", input))?;
        let close =
            find_matching_paren(trimmed, 0).ok_or_else(|| invalid_geom_input("path", input))?;
        if close + 1 == trimmed.len() {
            (true, inner.trim())
        } else {
            // Input like "(1,2),(3,4)" is a point sequence, not a wrapped path.
            (false, trimmed)
        }
    } else {
        if trimmed.ends_with(')') || trimmed.ends_with(']') {
            return Err(invalid_geom_input("path", input));
        }
        (false, trimmed)
    };

    if inner.is_empty() {
        return Err(invalid_geom_input("path", input));
    }

    let points = if inner.contains('(') {
        parse_point_sequence(inner, "path", input)?
    } else {
        let numbers = parse_number_list_even(inner, "path", input)?;
        numbers
            .chunks_exact(2)
            .map(|pair| GeomPoint {
                x: pair[0],
                y: pair[1],
            })
            .collect()
    };

    if points.is_empty() {
        return Err(invalid_geom_input("path", input));
    }

    Ok(GeomPath { closed, points })
}

pub(crate) fn parse_polygon_text(input: &str) -> DbResult<Vec<GeomPoint>> {
    let mut text = input.trim();
    if text.is_empty() {
        return Err(invalid_geom_input("polygon", input));
    }

    if starts_with_ascii_case_insensitive(text, "polygon(") {
        let open = text
            .find('(')
            .ok_or_else(|| invalid_geom_input("polygon", input))?;
        let close =
            find_matching_paren(text, open).ok_or_else(|| invalid_geom_input("polygon", input))?;
        if close + 1 != text.len() {
            return Err(invalid_geom_input("polygon", input));
        }
        text = text[open + 1..close].trim();
    }

    if let Some(inner) = text.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        let inner = inner.trim();
        if inner.starts_with('(') {
            let points = parse_point_sequence(inner, "polygon", input)?;
            if points.is_empty() {
                return Err(invalid_geom_input("polygon", input));
            }
            return Ok(points);
        }
    }

    if text.contains('(') {
        if let Ok(points) = parse_point_sequence(text, "polygon", input) {
            if points.is_empty() {
                return Err(invalid_geom_input("polygon", input));
            }
            return Ok(points);
        }
    }

    let numbers = parse_number_list_even(text, "polygon", input)?;
    if numbers.is_empty() {
        return Err(invalid_geom_input("polygon", input));
    }
    Ok(numbers
        .chunks_exact(2)
        .map(|pair| GeomPoint {
            x: pair[0],
            y: pair[1],
        })
        .collect())
}

pub(crate) fn parse_line_text(input: &str) -> DbResult<()> {
    parse_line_coefficients(input).map(|_| ())
}

pub(crate) fn parse_line_coefficients(input: &str) -> DbResult<(f64, f64, f64)> {
    let mut text = input.trim();
    if text.is_empty() {
        return Err(invalid_geom_input("line", input));
    }

    if let Some(inner) = text.strip_prefix("line(").and_then(|s| s.strip_suffix(')')) {
        text = inner.trim();
    }

    if let Some(inner) = text.strip_prefix('{') {
        let inner = inner
            .strip_suffix('}')
            .ok_or_else(|| invalid_geom_input("line", input))?;
        let coeffs = parse_number_list(inner.trim(), 3, "line", input)?;
        if coeffs[0] == 0.0 && coeffs[1] == 0.0 {
            return Err(invalid_line_spec("A and B cannot both be zero"));
        }
        return Ok((coeffs[0], coeffs[1], coeffs[2]));
    }

    if let Some(inner) = text.strip_prefix('[') {
        text = inner
            .strip_suffix(']')
            .map(str::trim)
            .ok_or_else(|| invalid_geom_input("line", input))?;
    } else if text.ends_with(']') {
        return Err(invalid_geom_input("line", input));
    }

    let points = if text.contains('(') {
        let text = strip_optional_outer_point_pair(text);
        parse_exactly_two_points(text, "line", input)?
    } else {
        let numbers = parse_number_list(text, 4, "line", input)?;
        [
            GeomPoint {
                x: numbers[0],
                y: numbers[1],
            },
            GeomPoint {
                x: numbers[2],
                y: numbers[3],
            },
        ]
    };

    if points[0].x == points[1].x && points[0].y == points[1].y {
        return Err(invalid_line_spec("must be two distinct points"));
    }
    let a = points[0].y - points[1].y;
    let b = points[1].x - points[0].x;
    let c = points[0].x * points[1].y - points[1].x * points[0].y;
    Ok((a, b, c))
}

pub(crate) fn parse_lseg_text(input: &str) -> DbResult<(GeomPoint, GeomPoint)> {
    let mut text = input.trim();
    if text.is_empty() {
        return Err(invalid_geom_input("lseg", input));
    }

    if starts_with_ascii_case_insensitive(text, "lseg(") {
        let open = text
            .find('(')
            .ok_or_else(|| invalid_geom_input("lseg", input))?;
        let close =
            find_matching_paren(text, open).ok_or_else(|| invalid_geom_input("lseg", input))?;
        if close + 1 != text.len() {
            return Err(invalid_geom_input("lseg", input));
        }
        text = text[open + 1..close].trim();
    }

    if let Some(inner) = text.strip_prefix('[') {
        text = inner
            .strip_suffix(']')
            .map(str::trim)
            .ok_or_else(|| invalid_geom_input("lseg", input))?;
    } else if text.ends_with(']') {
        return Err(invalid_geom_input("lseg", input));
    }

    if let Ok(numbers) = parse_number_list(text, 4, "lseg", input) {
        return Ok((
            GeomPoint {
                x: numbers[0],
                y: numbers[1],
            },
            GeomPoint {
                x: numbers[2],
                y: numbers[3],
            },
        ));
    }

    let text = strip_optional_outer_point_pair(text);
    let (first, rest) = parse_prefixed_point(text, "lseg", input)?;
    let rest = rest
        .trim_start()
        .strip_prefix(',')
        .map(str::trim_start)
        .ok_or_else(|| invalid_geom_input("lseg", input))?;
    let (second, tail) = parse_prefixed_point(rest, "lseg", input)?;
    if !tail.trim().is_empty() {
        return Err(invalid_geom_input("lseg", input));
    }
    Ok((first, second))
}

pub(crate) fn point_inside_box(point: GeomPoint, corner_a: GeomPoint, corner_b: GeomPoint) -> bool {
    let xmin = corner_a.x.min(corner_b.x);
    let xmax = corner_a.x.max(corner_b.x);
    let ymin = corner_a.y.min(corner_b.y);
    let ymax = corner_a.y.max(corner_b.y);
    point.x >= xmin && point.x <= xmax && point.y >= ymin && point.y <= ymax
}

pub(crate) fn parse_geometry_bounds(input: &str) -> DbResult<GeomBounds> {
    if let Ok((a, b)) = parse_box_text(input) {
        return Ok(bounds_from_points(&[a, b]));
    }
    if let Ok(point) = parse_point_text(input) {
        return Ok(bounds_from_points(&[point]));
    }
    if let Ok(polygon) = parse_polygon_text(input) {
        return Ok(bounds_from_points(&polygon));
    }
    if let Ok(path) = parse_path_text(input) {
        return Ok(bounds_from_points(&path.points));
    }
    if let Ok((a, b)) = parse_lseg_text(input) {
        return Ok(bounds_from_points(&[a, b]));
    }
    if let Ok(circle) = parse_circle_text(input) {
        return Ok(GeomBounds {
            xmin: circle.center.x - circle.radius,
            xmax: circle.center.x + circle.radius,
            ymin: circle.center.y - circle.radius,
            ymax: circle.center.y + circle.radius,
        });
    }
    Err(invalid_geom_input("geometry", input))
}

pub(crate) fn bounds_contains(outer: GeomBounds, inner: GeomBounds) -> bool {
    outer.xmin <= inner.xmin
        && outer.xmax >= inner.xmax
        && outer.ymin <= inner.ymin
        && outer.ymax >= inner.ymax
}

pub(crate) fn bounds_overlap(left: GeomBounds, right: GeomBounds) -> bool {
    left.xmin <= right.xmax
        && left.xmax >= right.xmin
        && left.ymin <= right.ymax
        && left.ymax >= right.ymin
}

pub(crate) fn bounds_strict_left(left: GeomBounds, right: GeomBounds) -> bool {
    left.xmax < right.xmin
}

pub(crate) fn bounds_strict_right(left: GeomBounds, right: GeomBounds) -> bool {
    left.xmin > right.xmax
}

fn bounds_from_points(points: &[GeomPoint]) -> GeomBounds {
    let mut xmin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for point in points {
        xmin = xmin.min(point.x);
        xmax = xmax.max(point.x);
        ymin = ymin.min(point.y);
        ymax = ymax.max(point.y);
    }
    GeomBounds {
        xmin,
        xmax,
        ymin,
        ymax,
    }
}

fn parse_exactly_two_points(
    text: &str,
    type_name: &str,
    full_input: &str,
) -> DbResult<[GeomPoint; 2]> {
    let (first, rest) = parse_prefixed_point(text, type_name, full_input)?;
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix(',')
        .map(str::trim_start)
        .ok_or_else(|| invalid_geom_input(type_name, full_input))?;
    let (second, tail) = parse_prefixed_point(rest, type_name, full_input)?;
    if !tail.trim().is_empty() {
        return Err(invalid_geom_input(type_name, full_input));
    }
    Ok([first, second])
}

fn parse_point_sequence(text: &str, type_name: &str, full_input: &str) -> DbResult<Vec<GeomPoint>> {
    let mut points = Vec::new();
    let mut remaining = text.trim();
    while !remaining.is_empty() {
        let (point, rest) = parse_prefixed_point(remaining, type_name, full_input)?;
        points.push(point);
        let rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        remaining = rest
            .strip_prefix(',')
            .map(str::trim_start)
            .ok_or_else(|| invalid_geom_input(type_name, full_input))?;
    }
    Ok(points)
}

fn parse_prefixed_point<'a>(
    text: &'a str,
    type_name: &str,
    full_input: &str,
) -> DbResult<(GeomPoint, &'a str)> {
    let trimmed = text.trim_start();

    if starts_with_ascii_case_insensitive(trimmed, "point(") {
        let open = trimmed
            .find('(')
            .ok_or_else(|| invalid_geom_input(type_name, full_input))?;
        let close = find_matching_paren(trimmed, open)
            .ok_or_else(|| invalid_geom_input(type_name, full_input))?;
        let coords = &trimmed[open + 1..close];
        let (x, y) = parse_two_numbers(coords, type_name, full_input)?;
        return Ok((GeomPoint { x, y }, &trimmed[close + 1..]));
    }

    if !trimmed.starts_with('(') {
        return Err(invalid_geom_input(type_name, full_input));
    }
    let close =
        find_matching_paren(trimmed, 0).ok_or_else(|| invalid_geom_input(type_name, full_input))?;
    let coords = &trimmed[1..close];
    let (x, y) = parse_two_numbers(coords, type_name, full_input)?;
    Ok((GeomPoint { x, y }, &trimmed[close + 1..]))
}

fn parse_two_numbers(text: &str, type_name: &str, full_input: &str) -> DbResult<(f64, f64)> {
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() != 2 {
        return Err(invalid_geom_input(type_name, full_input));
    }
    let x = parse_geom_number(parts[0], type_name, full_input)?;
    let y = parse_geom_number(parts[1], type_name, full_input)?;
    Ok((x, y))
}

fn parse_number_list(
    text: &str,
    expected: usize,
    type_name: &str,
    full_input: &str,
) -> DbResult<Vec<f64>> {
    let stripped = strip_single_outer_wrapping(text.trim());
    if stripped.contains('(') || stripped.contains(')') {
        return Err(invalid_geom_input(type_name, full_input));
    }
    let parts: Vec<&str> = stripped.split(',').collect();
    if parts.len() != expected {
        return Err(invalid_geom_input(type_name, full_input));
    }
    parts
        .into_iter()
        .map(|part| parse_geom_number(part, type_name, full_input))
        .collect()
}

fn parse_number_list_even(text: &str, type_name: &str, full_input: &str) -> DbResult<Vec<f64>> {
    let stripped = strip_single_outer_wrapping(text.trim());
    if stripped.contains('(') || stripped.contains(')') {
        return Err(invalid_geom_input(type_name, full_input));
    }
    let parts: Vec<&str> = stripped.split(',').collect();
    if parts.len() < 2 || parts.len() % 2 != 0 {
        return Err(invalid_geom_input(type_name, full_input));
    }
    parts
        .into_iter()
        .map(|part| parse_geom_number(part, type_name, full_input))
        .collect()
}

fn strip_single_outer_wrapping(text: &str) -> &str {
    if text.len() >= 2 {
        if let Some(inner) = text.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            return inner.trim();
        }
        if let Some(inner) = text.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            return inner.trim();
        }
    }
    text
}

fn strip_optional_outer_point_pair(text: &str) -> &str {
    let trimmed = text.trim();
    if !trimmed.starts_with("((") || !trimmed.ends_with("))") {
        return trimmed;
    }
    let inner = &trimmed[1..trimmed.len().saturating_sub(1)];
    let mut depth = 0_i32;
    for ch in inner.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return trimmed;
                }
            }
            _ => {}
        }
    }
    if depth == 0 {
        inner.trim()
    } else {
        trimmed
    }
}

fn parse_geom_number(token: &str, type_name: &str, full_input: &str) -> DbResult<f64> {
    let token = token.trim();
    match parse_geom_float(token) {
        Ok(value) => Ok(value),
        Err(FloatParseError::Invalid) => Err(invalid_geom_input(type_name, full_input)),
        Err(FloatParseError::OutOfRange(out_token)) => Err(double_out_of_range(&out_token)),
    }
}

enum FloatParseError {
    Invalid,
    OutOfRange(String),
}

fn parse_geom_float(token: &str) -> Result<f64, FloatParseError> {
    if token.is_empty() {
        return Err(FloatParseError::Invalid);
    }
    let lower = token.to_ascii_lowercase();
    if matches!(lower.as_str(), "nan" | "+nan" | "-nan") {
        return Ok(f64::NAN);
    }
    if matches!(lower.as_str(), "inf" | "+inf" | "infinity" | "+infinity") {
        return Ok(f64::INFINITY);
    }
    if matches!(lower.as_str(), "-inf" | "-infinity") {
        return Ok(f64::NEG_INFINITY);
    }

    match token.parse::<f64>() {
        Ok(value) => {
            if value.is_infinite() {
                return Err(FloatParseError::OutOfRange(token.to_owned()));
            }
            Ok(value)
        }
        Err(_) => {
            if token.parse::<NumericValue>().is_ok() {
                Err(FloatParseError::OutOfRange(token.to_owned()))
            } else {
                Err(FloatParseError::Invalid)
            }
        }
    }
}

fn find_matching_paren(text: &str, open_index: usize) -> Option<usize> {
    let mut depth = 0_i32;
    for (idx, ch) in text.char_indices().skip(open_index) {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn starts_with_ascii_case_insensitive(text: &str, prefix: &str) -> bool {
    match text.get(..prefix.len()) {
        Some(candidate) => candidate.eq_ignore_ascii_case(prefix),
        None => false,
    }
}

fn invalid_geom_input(type_name: &str, input: &str) -> DbError {
    DbError::invalid_input_syntax(type_name, input)
}

fn invalid_line_spec(detail: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::InvalidTextRepresentation,
        format!("invalid line specification: {detail}"),
    ))
}

fn double_out_of_range(token: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        format!("\"{token}\" is out of range for type double precision"),
    ))
}

#[cfg(test)]
mod tests {
    use super::{parse_box_text, starts_with_ascii_case_insensitive, validate_geometric_literal};

    #[test]
    fn box_parser_rejects_invalid_pg_regress_literals() {
        for literal in [
            "(2.3, 4.5)",
            "[1, 2, 3, 4)",
            "(1, 2, 3, 4]",
            "(1, 2, 3, 4) x",
            "asdfasdf(ad",
        ] {
            assert!(
                parse_box_text(literal).is_err(),
                "literal should be invalid: {literal}"
            );
        }
    }

    #[test]
    fn box_parser_accepts_adjacent_point_pair_syntax() {
        assert!(parse_box_text("(0,0)(0,100)").is_ok());
    }

    #[test]
    fn lseg_validation_is_enforced() {
        assert!(validate_geometric_literal("lseg", "[(1,2),(3,4)]").is_ok());
        assert!(validate_geometric_literal("lseg", "lseg(point(11,22),point(33,44))").is_ok());
        assert!(validate_geometric_literal("lseg", "[(1,2),(3,4)").is_err());
        assert!(validate_geometric_literal("lseg", "(3asdf,2 ,3,4r2)").is_err());
    }

    #[test]
    fn polygon_validation_rejects_circle_literal_shape() {
        assert!(validate_geometric_literal("polygon", "(0,1,2)").is_err());
    }

    #[test]
    fn ascii_prefix_check_handles_unicode_input_safely() {
        assert!(!starts_with_ascii_case_insensitive("ὀδυσσεύς", "circle("));
        assert!(starts_with_ascii_case_insensitive(
            "CIRCLE(1,2,3)",
            "circle("
        ));
    }
}
