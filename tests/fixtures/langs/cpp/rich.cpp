namespace shapes {

template <typename T>
class Box {
public:
	Box(T v) : value_(v) {}

	T get() const {
		return value_;
	}

	void set(T v) {
		value_ = v;
	}

private:
	T value_;
};

struct Pt {
	int x;
	int y;
};

template <typename T>
T twice(T v) {
	return v + v;
}

int total() {
	Box<int> b(2);
	b.set(twice(3));
	return b.get();
}

}  // namespace shapes

namespace shapes {
int pt_sum(Pt p) {
	return p.x + p.y;
}
}
