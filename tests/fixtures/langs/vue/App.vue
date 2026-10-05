<template>
	<div>{{ label }}</div>
</template>

<script lang="ts">
class Greeter {
	name: string;

	constructor(name: string) {
		this.name = name;
	}

	greet(): string {
		return "hello " + this.name;
	}
}

function run(): string {
	const g = new Greeter("sieve");
	return g.greet();
}

export default {
	computed: {
		label(): string {
			return run();
		},
	},
};
</script>
