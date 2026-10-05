local App = {}
App.__index = App

function App.new(name)
	local self = setmetatable({}, App)
	self.name = name
	return self
end

function App:greet()
	return "hello " .. self.name
end

local function run()
	local app = App.new("sieve")
	return app:greet()
end

return { run = run }
