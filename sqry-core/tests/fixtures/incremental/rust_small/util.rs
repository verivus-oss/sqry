//! Formatting helpers that depend on `shapes::Point`.

use crate::shapes::Point;

pub fn format_point(p: &Point) -> String {
    format!("({}, {})", p.x, p.y)
}

pub fn summarize(points: &[Point]) -> String {
    points
        .iter()
        .map(format_point)
        .collect::<Vec<_>>()
        .join(" | ")
}
