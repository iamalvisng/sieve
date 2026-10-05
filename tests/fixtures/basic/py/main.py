from py.helpers import add


class Calculator:
    def total(self, a, b):
        return add(a, b)


def run():
    calc = Calculator()
    return calc.total(1, 2)
