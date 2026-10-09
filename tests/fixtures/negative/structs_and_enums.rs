// Structs, enums, inherent impls and match: no category holds them.
use std::collections::HashMap;

const LIMIT: u32 = 10;
static NAME: &str = "inventory";

struct Item {
    name: String,
    count: u32,
}

enum Event {
    Added(Item),
    Removed { name: String },
    Cleared,
}

impl Item {
    fn new(name: String, count: u32) -> Item {
        Item { name, count }
    }

    fn label(&self) -> String {
        format!("{}: {}", self.name, self.count)
    }

    fn is_full(&self) -> bool {
        self.count >= LIMIT
    }
}

fn describe(event: Event) -> String {
    match event {
        Event::Added(item) => item.label(),
        Event::Removed { name } => format!("removed {name}"),
        Event::Cleared => NAME.to_string(),
    }
}

fn index(items: Vec<Item>) -> HashMap<String, u32> {
    let mut table = HashMap::new();
    for item in items {
        table.insert(item.name, item.count);
    }
    table
}

mod report {
    pub fn header() -> String {
        String::new()
    }
}
