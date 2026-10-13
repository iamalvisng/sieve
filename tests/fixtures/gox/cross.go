package gox

import (
	"example.com/gox/util"
	o "example.com/gox/other"
	"example.org/ext"
	"example.org/lone"
)

func Cross() {
	sib()
	helpers.Help()
	helpers.Twin()
	util.Help()
	o.Run()
	ext.Ext()
	lone.Lone()
	b := helpers.NewBox()
	b.Open()
	var c *helpers.Box
	c.Open()
	d := new(Local)
	d.Do()
	e := &Local{}
	e.Do()
}
