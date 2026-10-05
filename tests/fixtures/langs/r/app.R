library(R6)

greet <- function(x) {
	UseMethod("greet")
}

greet.default <- function(x) {
	paste("hello", x)
}

App <- R6Class("App", public = list(
	name = NULL,
	initialize = function(name) {
		self$name <- name
	},
	greet = function() {
		greet(self$name)
	}
))

run <- function() {
	app <- App$new("sieve")
	print(app$greet())
}
