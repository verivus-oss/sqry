//! Driver that exercises every cross-file edge in the fixture.

use rust_small::math::add;
use rust_small::shapes::Point;
use rust_small::util::format_point;

fn main() {
    let p = Point::new(3, 4);
    let sum = add(p.x, p.y);
    println!("{} has sum {}", format_point(&p), sum);
}
