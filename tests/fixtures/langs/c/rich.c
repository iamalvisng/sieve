typedef unsigned int uint;

typedef struct Point {
	int x;
	int y;
} Point;

enum Mode {
	MODE_A,
	MODE_B
};

union Value {
	int i;
	float f;
};

typedef int (*callback_t)(int);

static int twice(int v) {
	return v * 2;
}

int apply(callback_t cb, int v) {
	return cb(v);
}

int run_all(void) {
	int a = twice(3);
	return apply(twice, a);
}
