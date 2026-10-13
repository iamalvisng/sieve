package gox

type U struct{}

func (U) M() {}

type V struct{}

func (V) M() {}

type I interface{ M() }

func fn() {}

func ShadowParam(fn func()) { fn() }

func TypeParam[U I](x U) { x.M() }

func LocalType() {
	type U struct{ M func() }
	var x U
	x.M()
}

func TypeSwitch(v any) {
	switch x := v.(type) {
	case U:
		x.M()
	case V:
		x.M()
	}
}

func Select(ch chan func()) {
	select {
	case fn := <-ch:
		fn()
	}
}

func Iface(x I) { x.M() }
