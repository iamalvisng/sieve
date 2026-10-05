<?php

class Point {
	public static function origin(): Point {
		return new Point();
	}
}

class Base {
	public static function mk(): Base {
		return new Base();
	}
}

class Other {
	public static function mk(): Other {
		return new Other();
	}
}

class Child extends Base {
	public static function viaStatic(): Base {
		return static::mk();
	}

	public static function viaParent(): Base {
		return parent::mk();
	}
}

function helper(int $n): int {
	return $n + 1;
}

function build(Point $p, $obj) {
	$fn = fn($x) => $x * 2;
	$g = function ($y) {
		return helper($y);
	};
	array_map(function ($z) {
		return helper($z);
	}, [1, 2]);
	$h = function () {
		return function () {
			return helper(3);
		};
	};
	$o = Point::origin();
	$obj->m();
	return $fn;
}
