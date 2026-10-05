type app = { name : string }

let greet (a : app) : string = "hello " ^ a.name

module Runner = struct
	let run () =
		let a = { name = "sieve" } in
		greet a
end
