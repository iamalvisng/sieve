defmodule App do
	def greet(name) do
		"hello " <> name
	end

	def run do
		greet("sieve")
	end
end
