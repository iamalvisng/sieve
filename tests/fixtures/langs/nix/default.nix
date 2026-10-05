{ pkgs }:

let
	greet = name: "hello ${name}";
in
{
	run = name: greet name;
}
