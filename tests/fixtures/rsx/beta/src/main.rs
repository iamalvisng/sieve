mod m;
use m::hello;

fn main() {
    hello();
    alpha::exported();
    alpha::deepest();
}
