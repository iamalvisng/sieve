package gox

type T struct{}

func (t *T) M() {}

func (t *T) Self() { t.M() }

func NewT() *T { return &T{} }

func helper() {}

func Proofs(p *T) {
	helper()
	p.M()
	var a T
	a.M()
	b := T{}
	b.M()
	c := &T{}
	c.M()
	d := NewT()
	d.M()
}
