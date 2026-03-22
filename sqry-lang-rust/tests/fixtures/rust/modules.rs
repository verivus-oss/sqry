// Test fixture: Module structure
// Tests: utils::helper, utils::process, data::Record::new

mod utils {
    pub fn helper(x: i32) -> i32 {
        x * 2
    }

    pub fn process(data: &str) -> String {
        helper(data.len() as i32);
        data.to_uppercase()
    }

    pub mod nested {
        pub fn transform(value: i32) -> i32 {
            super::helper(value) + 10
        }
    }
}

mod data {
    pub struct Record {
        pub id: u32,
        pub name: String,
    }

    impl Record {
        pub fn new(id: u32, name: &str) -> Self {
            Record {
                id,
                name: name.to_string(),
            }
        }

        pub fn display(&self) {
            println!("Record #{}: {}", self.id, self.name);
        }
    }
}

mod processing {
    use super::data::Record;
    use super::utils;

    pub fn process_record(record: &Record) -> String {
        let name_upper = utils::process(&record.name);
        format!("{}-{}", record.id, name_upper)
    }
}

fn main() {
    let result1 = utils::helper(21);
    println!("Helper result: {}", result1);

    let result2 = utils::process("hello");
    println!("Process result: {}", result2);

    let result3 = utils::nested::transform(5);
    println!("Nested transform: {}", result3);

    let record = data::Record::new(1, "test");
    record.display();

    let processed = processing::process_record(&record);
    println!("Processed record: {}", processed);
}
