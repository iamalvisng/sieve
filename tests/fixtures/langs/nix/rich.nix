let
	double = x: x * 2;
	pair = { a = 1; b = 2; };
	nested = {
		inner = {
			triple = x: x * 3;
		};
		quad = x: double (double x);
	};
in
{
	total = double 4;
	inner = nested.inner;
	run = x: nested.quad (double x);
}
