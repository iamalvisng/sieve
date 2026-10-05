def _helper():
    pass


class _Private:
    pass


class Public:
    def __init__(self):
        pass

    @staticmethod
    def make():
        pass

    @property
    def size(self):
        return 1

    def outer(self):
        def inner():
            pass
        return inner()
