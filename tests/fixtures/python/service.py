"""Fixture: every member kind the Python adapter knows about."""

MAX_RETRIES = 3

handler = lambda value: value


def plain(value):
    def nested(inner):
        return inner

    return nested(value)


async def fetch(url):
    return url


class Service:
    DEFAULT = "d"

    def __init__(self, name):
        self.name = name

    @property
    def name(self):
        return self._name

    async def run(self):
        return self.name


def outer():
    LOCAL_LIMIT = 5
    return LOCAL_LIMIT


@decorator
def decorated():
    return None
