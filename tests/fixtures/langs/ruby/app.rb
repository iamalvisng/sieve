require "set"

module Named
	def label
		"app"
	end
end

class App
	include Named

	def initialize(name)
		@name = name
	end

	def greet
		"hello #{@name}"
	end
end

def run
	app = App.new("sieve")
	app.greet
end
