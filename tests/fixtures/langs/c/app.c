#include <stdio.h>
#include "util.h"

struct App {
	char name[32];
};

const char *greet(struct App *app) {
	return app->name;
}

int main(void) {
	struct App app;
	snprintf(app.name, sizeof(app.name), "sieve");
	printf("hello %s\n", greet(&app));
	famB_run();
	return 0;
}
