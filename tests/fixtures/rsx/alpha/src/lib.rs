mod deep;
mod util;
pub use deep::inner::deepest;

pub fn exported() {}

fn run() {
    util::clean();
    deepest();
}
