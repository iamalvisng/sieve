package shadow

type T struct{}

func (t *T) M() {}

func Use() {
	d := new(T)
	d.M()
}
