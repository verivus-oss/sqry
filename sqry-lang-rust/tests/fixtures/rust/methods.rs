// Test fixture: Method calls with self resolution
// Tests: Widget::new, Widget::update, Widget::process with self.update()

struct Widget {
    name: String,
    value: i32,
}

impl Widget {
    fn new(name: &str) -> Self {
        Widget {
            name: name.to_string(),
            value: 0,
        }
    }

    fn update(&mut self, delta: i32) {
        self.value += delta;
    }

    fn get_value(&self) -> i32 {
        self.value
    }

    fn process(&mut self, increment: i32) -> i32 {
        self.update(increment);
        self.get_value()
    }
}

fn main() {
    let mut widget = Widget::new("TestWidget");
    widget.update(10);

    let result = widget.process(5);
    println!("Widget {} has value: {}", widget.name, result);
}
