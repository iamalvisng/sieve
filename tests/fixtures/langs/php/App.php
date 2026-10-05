<?php

use Foo\Bar;
use Models\User;

interface Greeter {
	public function greet(): string;
}

trait Named {
	public function label(): string {
		return "app";
	}
}

class App implements Greeter {
	use Named;

	private string $name;

	public function __construct(string $name) {
		$this->name = $name;
	}

	public function greet(): string {
		return "hello " . $this->name;
	}
}

function run(): void {
	$app = new App("sieve");
	echo $app->greet();
}
