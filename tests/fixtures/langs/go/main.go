package main

import (
	"fmt"

	"example.com/langs/go/greet"
)

type Greeter struct {
	Name string
}

func (g Greeter) Greet() string {
	return fmt.Sprintf("hello %s", g.Name)
}

func main() {
	g := Greeter{Name: "sieve"}
	fmt.Println(g.Greet())
	_ = greet.Hello
}
