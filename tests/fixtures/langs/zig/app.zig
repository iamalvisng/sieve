const std = @import("std");

const App = struct {
	name: []const u8,

	fn greet(self: App) []const u8 {
		return self.name;
	}
};

fn run() []const u8 {
	const app = App{ .name = "sieve" };
	return app.greet();
}
