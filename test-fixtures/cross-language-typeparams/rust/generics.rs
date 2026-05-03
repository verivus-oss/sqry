use std::fmt::Display;

pub trait Store<T>
where
    T: Clone,
{
    fn put(&mut self, value: T);
}

pub fn identity<T: Clone>(value: T) -> T {
    value.clone()
}

pub fn constrained<T>(value: T) -> T
where
    T: Display + Send,
{
    value
}

pub fn array<const N: usize>() -> [u8; N] {
    [0; N]
}
