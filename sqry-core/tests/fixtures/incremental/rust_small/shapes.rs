//! Geometry types used by the fixture.

use crate::math::square;

pub struct Point {
    pub x: i64,
    pub y: i64,
}

impl Point {
    pub fn new(x: i64, y: i64) -> Self {
        Point { x, y }
    }

    pub fn distance_squared_from_origin(&self) -> i64 {
        square(self.x) + square(self.y)
    }
}
