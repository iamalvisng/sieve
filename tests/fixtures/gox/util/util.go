package helpers

type Box struct{}

func (b *Box) Open() {}

func NewBox() *Box { return &Box{} }

func Help() {}

// UseTest calls a func that only a test file defines.
func UseTest() { TestOnly() }

// CallTwin calls a func that two build-tag files define.
func CallTwin() { Twin() }
