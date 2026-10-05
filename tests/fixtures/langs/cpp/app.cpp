#include <iostream>
#include <string>

namespace app {

class Greeter {
public:
	Greeter(std::string name) : name_(name) {}

	std::string greet() {
		return "hello " + name_;
	}

private:
	std::string name_;
};

std::string run() {
	Greeter g("sieve");
	return g.greet();
}

}  // namespace app

int main() {
	std::cout << app::run() << std::endl;
	return 0;
}

void famB_run() {
}
