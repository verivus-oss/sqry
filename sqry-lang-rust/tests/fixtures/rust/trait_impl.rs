// Test fixture: Trait implementations
// Tests: Widget impl Display with show method, Widget::process calls self.show()

use std::fmt;

struct Widget {
    name: String,
    value: i32,
}

impl Widget {
    fn new(name: &str, value: i32) -> Self {
        Widget {
            name: name.to_string(),
            value,
        }
    }

    fn show(&self) {
        println!("Widget: {} = {}", self.name, self.value);
    }

    fn process(&self) -> i32 {
        self.show();
        self.value * 2
    }
}

impl fmt::Display for Widget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Widget({}: {})", self.name, self.value)
    }
}

trait Processable {
    fn compute(&self) -> i32;
    fn validate(&self) -> bool;
}

impl Processable for Widget {
    fn compute(&self) -> i32 {
        self.value * 10
    }

    fn validate(&self) -> bool {
        self.value > 0
    }
}

fn use_trait<T: Processable>(item: &T) -> i32 {
    if item.validate() {
        item.compute()
    } else {
        0
    }
}

fn main() {
    let widget = Widget::new("Test", 42);

    println!("Display: {}", widget);

    let result = widget.process();
    println!("Process result: {}", result);

    let computed = use_trait(&widget);
    println!("Trait method result: {}", computed);
}
