//! Fixture: every member kind the Rust adapter knows about.

pub const CAPACITY: usize = 16;

static DEFAULT_NAME: &str = "cache";

pub type Slot = usize;

pub struct Cache {
    entries: Vec<Slot>,
}

pub enum State {
    Empty,
    Full,
}

pub trait Store {
    fn put(&mut self, slot: Slot) -> bool {
        true
    }
}

impl Cache {
    pub fn new() -> Self {
        Cache { entries: Vec::new() }
    }

    fn helper() {
        fn local() {}
    }
}

impl Store for Cache {
    fn put(&mut self, _slot: Slot) -> bool {
        false
    }
}

pub mod inner {
    pub fn deep() {}
}

macro_rules! twice {
    ($x:expr) => {
        $x * 2
    };
}
