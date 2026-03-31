macro_rules! simple_macro {
    ($x:expr) => {
        $x + 1
    };
}

macro_rules! multi_metavar {
    ($x:expr, $y:ty, $z:ident) => {
        let $z: $y = $x;
    };
}

macro_rules! repeated_macro {
    ($($x:expr),*) => {
        vec![$($x),*]
    };
}

macro_rules! multi_arm {
    ($x:expr) => {
        $x
    };
    ($x:expr, $y:expr) => {
        $x + $y
    };
}

macro_rules! nested_repeat {
    ($($key:expr => $value:expr),*) => {
        {
            let mut map = std::collections::HashMap::new();
            $(map.insert($key, $value);)*
            map
        }
    };
}

macro_rules! no_metavars {
    () => {
        42
    };
}

// Recursive macro definition
macro_rules! outer_macro {
    ($name:ident) => {
        macro_rules! $name {
            () => {
                "inner"
            };
        }
    };
}
